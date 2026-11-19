//! yt-dlp aktuell halten: Update auf Knopfdruck und einmal täglich zur
//! eingestellten Uhrzeit. YouTube ändert sich ständig – eine veraltete
//! yt-dlp-Version ist die häufigste Ursache für „lädt nicht mehr“.
//!
//! Jede neue Version wird geprüft (Audio-Link eines Referenz-Videos
//! auflösen). Schlägt das fehl, die alte Version aber funktioniert, geht es
//! automatisch auf die alte zurück. Das gilt auch für das Update, das der
//! Entrypoint beim Containerstart macht: Beim Start wird die Version mit der
//! zuletzt geprüften verglichen.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use jiff::{Timestamp, Zoned, civil::Date, tz::TimeZone};
use lemusic_core::{Db, SettingsStore};
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Mutex};
use tracing::{info, warn};

use crate::metrics;

/// Zeitzone für den Zeitplan, falls `TZ` nicht gesetzt ist.
const DEFAULT_TZ: &str = "Europe/Berlin";
/// Datenbank-Schlüssel für das Ergebnis des letzten Laufs.
const STATUS_KEY: &str = "ytdlp:last_update";
/// Datenbank-Schlüssel der zuletzt erfolgreich geprüften Version.
const GOOD_KEY: &str = "ytdlp:good_version";
/// Höchstdauer für pip.
const PIP_TIMEOUT: Duration = Duration::from_secs(180);
/// Referenz für die Prüfung: „Me at the zoo“, das erste YouTube-Video
/// (seit 2005 online, kurz, ohne Altersgrenze).
const CHECK_URL: &str = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
const CHECK_TIMEOUT: Duration = Duration::from_secs(90);
/// Paket samt Extras, wie im Dockerfile.
const PACKAGE: &str = "yt-dlp[default,deno]";

/// Nie zwei Updates gleichzeitig (Knopf + Zeitplan).
static RUNNING: Mutex<()> = Mutex::const_new(());

/// Ergebnis des letzten Update-Laufs (für die Weboberfläche).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// Zeitpunkt, lesbar in der eingestellten Zeitzone.
    pub at: String,
    /// „manuell“ oder „täglich“.
    pub trigger: String,
    pub ok: bool,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Kurzbeschreibung bzw. Fehlermeldung.
    pub message: String,
}

/// Zeitzone aus `TZ`, sonst Europe/Berlin.
pub fn time_zone() -> TimeZone {
    std::env::var("TZ")
        .ok()
        .filter(|tz| !tz.trim().is_empty())
        .and_then(|tz| TimeZone::get(tz.trim()).ok())
        .or_else(|| TimeZone::get(DEFAULT_TZ).ok())
        .unwrap_or(TimeZone::UTC)
}

/// Name der verwendeten Zeitzone (für die Anzeige).
pub fn time_zone_name() -> String {
    time_zone()
        .iana_name()
        .map(str::to_string)
        .unwrap_or_else(|| "UTC".into())
}

fn now() -> Zoned {
    Timestamp::now().to_zoned(time_zone())
}

/// Installierte yt-dlp-Version.
pub async fn version() -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("yt-dlp")
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `pip install --upgrade yt-dlp[…]`
async fn pip_upgrade() -> Result<()> {
    pip_install(&["--upgrade", PACKAGE]).await
}

/// Genau diese Version installieren (für den Rückfall).
async fn pip_pin(version: &str) -> Result<()> {
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.')
    {
        bail!("Ungültige Versionsangabe: {version}");
    }
    pip_install(&[&format!("{PACKAGE}=={version}")]).await
}

async fn pip_install(args: &[&str]) -> Result<()> {
    let output = tokio::time::timeout(
        PIP_TIMEOUT,
        Command::new("pip")
            .args([
                "install",
                "--no-cache-dir",
                "--quiet",
                "--disable-pip-version-check",
            ])
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Update dauerte zu lange")?
    .context("pip nicht ausführbar")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Update fehlgeschlagen: {}",
            stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("unbekannter Fehler")
        );
    }
    Ok(())
}

/// Funktioniert yt-dlp? Löst den Audio-Link des Referenz-Videos auf – genau
/// der Teil, der bei YouTube-Änderungen als Erstes bricht.
pub async fn check_extraction() -> Result<()> {
    let output = tokio::time::timeout(
        CHECK_TIMEOUT,
        Command::new("yt-dlp")
            .args(crate::ytdlp_base_args())
            .args([
                "--quiet",
                "--no-warnings",
                "--no-playlist",
                "-f",
                "bestaudio/best",
                "--get-url",
                "--",
                CHECK_URL,
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Prüfung dauerte zu lange")?
    .context("yt-dlp nicht ausführbar")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.trim_start().starts_with("http") {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "{}",
        stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("keine Audio-Adresse erhalten")
    )
}

/// Was nach einer Prüfung zu tun ist (reine Entscheidung, testbar).
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Neue Version funktioniert.
    Keep,
    /// Neue kaputt, alte funktioniert → alte behalten.
    RolledBack,
    /// Beide scheitern → liegt nicht an der Version (Netz, YouTube); die
    /// neue bleibt, weil sie eher einen Fix enthält.
    BothFail,
    /// Neue kaputt, keine alte bekannt.
    NoFallback,
}

fn verdict(new_ok: bool, old: Option<bool>) -> Verdict {
    match (new_ok, old) {
        (true, _) => Verdict::Keep,
        (false, Some(true)) => Verdict::RolledBack,
        (false, Some(false)) => Verdict::BothFail,
        (false, None) => Verdict::NoFallback,
    }
}

/// Prüft die installierte Version `new` und fällt bei Bedarf auf `old`
/// zurück. Liefert (ok, Meldung).
async fn verify_or_rollback(db: &Db, old: Option<&str>, new: &str) -> (bool, String) {
    let new_err = match check_extraction().await {
        Ok(()) => {
            remember_good(db, new).await;
            return (true, format!("Version {new} geprüft"));
        }
        Err(e) => format!("{e:#}"),
    };
    warn!(version = new, error = %new_err, "Neue yt-dlp-Version besteht die Prüfung nicht");
    let old_ok = match old.filter(|o| *o != new) {
        None => None,
        Some(old) => match pip_pin(old).await {
            Ok(()) => Some(check_extraction().await.is_ok()),
            Err(e) => {
                warn!(error = %e, "Alte yt-dlp-Version nicht installierbar");
                None
            }
        },
    };
    match (verdict(false, old_ok), old) {
        (Verdict::RolledBack, Some(old)) => {
            metrics::YTDLP_ROLLBACKS.inc();
            remember_good(db, old).await;
            (
                false,
                format!("Version {new} funktioniert nicht ({new_err}) – zurück auf {old}"),
            )
        }
        (Verdict::BothFail, _) => {
            if let Err(e) = pip_pin(new).await {
                warn!(error = %e, "Neue yt-dlp-Version nicht wieder installierbar");
            }
            (
                false,
                format!(
                    "Prüfung scheitert auch mit der alten Version ({new_err}) – vermutlich \
                     Netzwerk oder YouTube; {new} bleibt installiert"
                ),
            )
        }
        _ => (
            false,
            format!("Version {new} besteht die Prüfung nicht: {new_err}"),
        ),
    }
}

async fn remember_good(db: &Db, version: &str) {
    if let Err(e) = db.set_json(GOOD_KEY, &version).await {
        warn!(error = %e, "Geprüfte yt-dlp-Version nicht gespeichert");
    }
}

async fn save_status(db: &Db, status: &UpdateStatus) {
    if let Err(e) = db.set_json(STATUS_KEY, status).await {
        warn!(error = %e, "yt-dlp-Status nicht gespeichert");
    }
}

fn log_result(trigger: &str, ok: bool, message: &str) {
    if ok {
        info!(trigger, "yt-dlp: {message}");
    } else {
        warn!(trigger, error = %message, "yt-dlp-Update nicht erfolgreich");
    }
}

/// Prüft auf eine neue Version, installiert und prüft sie (mit Rückfall auf
/// die alte, wenn die neue nicht funktioniert). Speichert das Ergebnis.
pub async fn update(db: &Db, trigger: &str) -> UpdateStatus {
    let _guard = RUNNING.lock().await;
    metrics::YTDLP_UPDATES.inc();
    let before = version().await;
    let result = pip_upgrade().await;
    let installed = version().await;
    let (ok, message) = match (result, installed.as_deref()) {
        (Err(e), _) => (false, format!("{e:#}")),
        (Ok(()), _) if before.is_some() && before == installed => {
            (true, "Bereits die neueste Version".to_string())
        }
        (Ok(()), None) => (false, "yt-dlp nach dem Update nicht ausführbar".to_string()),
        (Ok(()), Some(new)) => {
            let (ok, checked) = verify_or_rollback(db, before.as_deref(), new).await;
            let head = format!("Aktualisiert: {} → {new}", before.as_deref().unwrap_or("?"));
            (ok, format!("{head} · {checked}"))
        }
    };
    if !ok {
        metrics::YTDLP_UPDATE_FAILURES.inc();
    }
    let after = version().await;
    log_result(trigger, ok, &message);
    let status = UpdateStatus {
        at: now().strftime("%d.%m.%Y %H:%M").to_string(),
        trigger: trigger.into(),
        ok,
        before,
        after,
        message,
    };
    save_status(db, &status).await;
    status
}

/// Nach dem Start: Hat der Entrypoint eine neue Version installiert, wird sie
/// geprüft (und ggf. auf die zuletzt geprüfte zurückgesetzt).
pub async fn verify_after_start(db: &Db) {
    let _guard = RUNNING.lock().await;
    let Some(current) = version().await else {
        warn!("yt-dlp nicht ausführbar");
        return;
    };
    let good: Option<String> = db.get_json(GOOD_KEY).await.ok().flatten();
    if good.as_deref() == Some(current.as_str()) {
        return;
    }
    let (ok, message) = verify_or_rollback(db, good.as_deref(), &current).await;
    if !ok {
        metrics::YTDLP_UPDATE_FAILURES.inc();
    }
    log_result("Start", ok, &message);
    let status = UpdateStatus {
        at: now().strftime("%d.%m.%Y %H:%M").to_string(),
        trigger: "Start".into(),
        ok,
        before: good,
        after: version().await,
        message,
    };
    save_status(db, &status).await;
}

/// Ergebnis des letzten Laufs.
pub async fn last_status(db: &Db) -> Option<UpdateStatus> {
    db.get_json(STATUS_KEY).await.ok().flatten()
}

/// „HH:MM“ → (Stunde, Minute).
pub fn parse_time(s: &str) -> Option<(i8, i8)> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: i8 = h.parse().ok()?;
    let m: i8 = m.parse().ok()?;
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

/// Ist es Zeit für den täglichen Lauf (und lief er heute noch nicht)?
fn due(now: &Zoned, at: (i8, i8), last_run: Option<Date>) -> bool {
    let (h, m) = at;
    let reached = (now.hour(), now.minute()) >= (h, m);
    reached && last_run != Some(now.date())
}

/// Startet den täglichen Zeitplan.
pub fn spawn_daily(db: Db, settings: SettingsStore) {
    tokio::spawn(async move {
        // Beim Containerstart aktualisiert schon das Entrypoint-Skript –
        // heute also erst wieder, wenn die Uhrzeit noch bevorsteht.
        let started = now();
        let mut last_run: Option<Date> = None;
        if let Some((h, m)) = parse_time(&settings.current().ytdlp_update_time)
            && (started.hour(), started.minute()) >= (h, m)
        {
            last_run = Some(started.date());
        }
        info!(
            time_zone = %time_zone_name(),
            "Tägliches yt-dlp-Update eingeplant"
        );
        verify_after_start(&db).await;
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let current = settings.current();
            let Some(at) = parse_time(&current.ytdlp_update_time) else {
                continue;
            };
            let now = now();
            if !current.ytdlp_auto_update {
                continue;
            }
            if due(&now, at, last_run) {
                last_run = Some(now.date());
                update(&db, "täglich").await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_decisions() {
        assert_eq!(verdict(true, None), Verdict::Keep);
        assert_eq!(verdict(true, Some(false)), Verdict::Keep);
        assert_eq!(verdict(false, Some(true)), Verdict::RolledBack);
        assert_eq!(verdict(false, Some(false)), Verdict::BothFail);
        assert_eq!(verdict(false, None), Verdict::NoFallback);
    }

    #[test]
    fn times_are_parsed() {
        assert_eq!(parse_time("04:00"), Some((4, 0)));
        assert_eq!(parse_time("4:05"), Some((4, 5)));
        assert_eq!(parse_time("23:59"), Some((23, 59)));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:5"), None);
        assert_eq!(parse_time("ab:cd"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn runs_once_per_day_after_the_time() -> Result<()> {
        let tz = TimeZone::get("Europe/Berlin")?;
        let at = |s: &str| -> Result<Zoned> {
            Ok(s.parse::<jiff::civil::DateTime>()?.to_zoned(tz.clone())?)
        };
        let before = at("2026-11-17T03:59")?;
        let on_time = at("2026-11-17T04:00")?;
        let later = at("2026-11-17T09:30")?;
        assert!(!due(&before, (4, 0), None));
        assert!(due(&on_time, (4, 0), None));
        // Verpasst (z. B. Container lief nicht) → später am Tag nachholen.
        assert!(due(&later, (4, 0), None));
        // Heute schon gelaufen → nicht nochmal.
        assert!(!due(&later, (4, 0), Some(later.date())));
        // Am nächsten Tag wieder.
        let tomorrow = at("2026-11-18T04:00")?;
        assert!(due(&tomorrow, (4, 0), Some(later.date())));
        Ok(())
    }
}
