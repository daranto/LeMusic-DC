//! yt-dlp aktuell halten: Update auf Knopfdruck und einmal täglich zur
//! eingestellten Uhrzeit. YouTube ändert sich ständig – eine veraltete
//! yt-dlp-Version ist die häufigste Ursache für „lädt nicht mehr“.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use jiff::{Timestamp, Zoned, civil::Date, tz::TimeZone};
use lemusic_core::{Db, SettingsStore};
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::Mutex};
use tracing::{info, warn};

/// Zeitzone für den Zeitplan, falls `TZ` nicht gesetzt ist.
const DEFAULT_TZ: &str = "Europe/Berlin";
/// Datenbank-Schlüssel für das Ergebnis des letzten Laufs.
const STATUS_KEY: &str = "ytdlp:last_update";
/// Höchstdauer für pip.
const PIP_TIMEOUT: Duration = Duration::from_secs(180);

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

async fn pip_upgrade() -> Result<()> {
    let output = tokio::time::timeout(
        PIP_TIMEOUT,
        Command::new("pip")
            .args([
                "install",
                "--no-cache-dir",
                "--quiet",
                "--disable-pip-version-check",
                "--upgrade",
                "yt-dlp[default,deno]",
            ])
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

/// Prüft auf eine neue Version und installiert sie. Speichert das Ergebnis.
pub async fn update(db: &Db, trigger: &str) -> UpdateStatus {
    let _guard = RUNNING.lock().await;
    let before = version().await;
    let result = pip_upgrade().await;
    let after = version().await;
    let (ok, message) = match result {
        Err(e) => (false, format!("{e:#}")),
        Ok(()) if before.is_some() && before == after => {
            (true, "Bereits die neueste Version".to_string())
        }
        Ok(()) => (
            true,
            format!(
                "Aktualisiert: {} → {}",
                before.as_deref().unwrap_or("?"),
                after.as_deref().unwrap_or("?")
            ),
        ),
    };
    if ok {
        info!(trigger, before = ?before, after = ?after, "yt-dlp: {message}");
    } else {
        warn!(trigger, error = %message, "yt-dlp-Update fehlgeschlagen");
    }
    let status = UpdateStatus {
        at: now().strftime("%d.%m.%Y %H:%M").to_string(),
        trigger: trigger.into(),
        ok,
        before,
        after,
        message,
    };
    if let Err(e) = db.set_json(STATUS_KEY, &status).await {
        warn!(error = %e, "yt-dlp-Status nicht gespeichert");
    }
    status
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
