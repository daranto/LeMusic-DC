//! `/health` (Docker-Healthcheck, Übersicht) und `/metrics` (Prometheus).

use std::{
    fmt::Write as _,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use lemusic_player::{Snapshot, metrics as m};
use serde::Serialize;

use crate::WebState;

/// So lange darf die Position eines laufenden Tracks stillstehen, bevor die
/// Voice-Verbindung als gestört gilt. Der Watchdog im Player greift schon
/// nach 20 s – das hier fängt den Fall ab, dass selbst er nicht hilft.
const VOICE_STALL: Duration = Duration::from_secs(60);
/// Antwortzeit, die dem Player-Actor zugestanden wird.
const PLAYER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize)]
pub(crate) struct HealthReport {
    status: &'static str,
    discord: bool,
    database: bool,
    /// Player-Actor antwortet.
    player: bool,
    /// Läuft gerade etwas, kommt es auch an (verbunden, Position bewegt sich).
    voice: bool,
    /// yt-dlp ausführbar.
    ytdlp: bool,
    version: String,
}

/// Letzte beobachtete Wiedergabeposition (für `voice`).
#[derive(Default)]
pub(crate) struct VoiceWatch(Mutex<Option<Progress>>);

#[derive(Clone, Copy)]
struct Progress {
    entry: u64,
    position: Duration,
    since: Instant,
}

impl VoiceWatch {
    fn check(&self, snapshot: &Snapshot, now: Instant) -> bool {
        let Ok(mut last) = self.0.lock() else {
            return true;
        };
        voice_ok(&mut last, snapshot, now)
    }
}

/// Spielt etwas (bereit, nicht pausiert), muss der Bot verbunden sein und
/// die Position sich innerhalb von [`VOICE_STALL`] bewegen.
fn voice_ok(last: &mut Option<Progress>, snapshot: &Snapshot, now: Instant) -> bool {
    let Some(playing) = snapshot.now.as_ref().filter(|n| n.ready && !n.paused) else {
        *last = None;
        return true;
    };
    if snapshot.voice_channel.is_none() {
        return false;
    }
    match last {
        Some(p) if p.entry == playing.entry.id && p.position == playing.position => {
            now.duration_since(p.since) < VOICE_STALL
        }
        _ => {
            *last = Some(Progress {
                entry: playing.entry.id,
                position: playing.position,
                since: now,
            });
            true
        }
    }
}

pub(crate) async fn health_report(state: &WebState) -> (bool, HealthReport) {
    let database = tokio::time::timeout(Duration::from_secs(2), state.db.ping())
        .await
        .is_ok_and(|r| r.is_ok());
    let discord = state.health.discord_connected();
    let snapshot = tokio::time::timeout(
        PLAYER_TIMEOUT,
        state.players.player(state.guild_id).snapshot(),
    )
    .await
    .ok()
    .and_then(Result::ok);
    let player = snapshot.is_some();
    let voice = snapshot
        .as_ref()
        .is_none_or(|s| state.voice_watch.check(s, Instant::now()));
    let ytdlp = crate::api::ytdlp_version(false).await != crate::api::YTDLP_MISSING;
    let ok = database && discord && player && voice && ytdlp;
    (
        ok,
        HealthReport {
            status: if ok { "ok" } else { "degraded" },
            discord,
            database,
            player,
            voice,
            ytdlp,
            version: lemusic_core::config::version_string(),
        },
    )
}

pub(crate) async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let (ok, report) = health_report(&state).await;
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(report))
}

/// Prometheus-Textformat (Version 0.0.4).
pub(crate) async fn metrics(State(state): State<WebState>) -> Response {
    let (_, health) = health_report(&state).await;
    let snapshot = state.players.player(state.guild_id).snapshot().await.ok();
    let cache = tokio::task::spawn_blocking(lemusic_player::cache::stats)
        .await
        .ok();

    let mut out = Metrics::default();
    out.gauge(
        "lemusic_up",
        "Alles in Ordnung (wie /health)",
        health.status == "ok",
    );
    out.gauge(
        "lemusic_discord_connected",
        "Mit Discord verbunden",
        health.discord,
    );
    out.gauge(
        "lemusic_database_ok",
        "Datenbank erreichbar",
        health.database,
    );
    out.gauge("lemusic_player_ok", "Player-Actor antwortet", health.player);
    out.gauge("lemusic_voice_ok", "Wiedergabe kommt an", health.voice);
    out.gauge("lemusic_ytdlp_ok", "yt-dlp ausführbar", health.ytdlp);
    out.gauge(
        "lemusic_uptime_seconds",
        "Laufzeit des Prozesses",
        state.started.elapsed().as_secs(),
    );
    if let Some(rss) = resident_bytes() {
        out.gauge(
            "lemusic_memory_resident_bytes",
            "Belegter Arbeitsspeicher",
            rss,
        );
    }

    if let Some(s) = &snapshot {
        let playing = s.now.as_ref().is_some_and(|n| !n.paused);
        out.gauge("lemusic_playing", "Es läuft gerade etwas", playing);
        let requests = s.queue.iter().filter(|e| !e.autoplay).count();
        let guests = s.queue.iter().filter(|e| e.guest).count();
        out.gauge("lemusic_queue_requests", "Wartende Wünsche", requests);
        out.gauge(
            "lemusic_queue_guest_requests",
            "Wartende Gast-Wünsche",
            guests,
        );
        out.gauge(
            "lemusic_queue_autoplay",
            "Wartende Autoplaylist-Titel",
            s.queue.len() - requests,
        );
    }

    out.counter(
        "lemusic_tracks_started_total",
        "Gestartete Titel",
        m::TRACKS_STARTED.get(),
    );
    out.counter(
        "lemusic_tracks_finished_total",
        "Vollständig gespielte Titel",
        m::TRACKS_FINISHED.get(),
    );
    out.counter(
        "lemusic_tracks_failed_total",
        "Übersprungene Titel (nach Wiederholungen)",
        m::TRACKS_FAILED.get(),
    );
    out.counter(
        "lemusic_track_retries_total",
        "Wiederholungen nach einem Fehler",
        m::TRACK_RETRIES.get(),
    );
    out.counter(
        "lemusic_stream_stalls_total",
        "Hängende Streams (Watchdog)",
        m::STREAM_STALLS.get(),
    );
    out.summary(
        "lemusic_track_load_seconds",
        "Zeit bis ein Titel abspielbereit ist",
        m::TRACK_LOAD.sum_secs(),
        m::TRACK_LOAD.count(),
    );
    out.counter(
        "lemusic_audio_ticks_total",
        "Gemessene 20-ms-Audiotakte",
        m::AUDIO_TICKS.get(),
    );
    out.counter(
        "lemusic_audio_late_wakeups_total",
        "Verspätete Audiotakte (> 15 ms, mögliche Aussetzer)",
        m::AUDIO_LATE_WAKEUPS.get(),
    );
    out.counter(
        "lemusic_ytdlp_updates_total",
        "yt-dlp-Update-Läufe",
        m::YTDLP_UPDATES.get(),
    );
    out.counter(
        "lemusic_ytdlp_update_failures_total",
        "Nicht erfolgreiche yt-dlp-Updates",
        m::YTDLP_UPDATE_FAILURES.get(),
    );
    out.counter(
        "lemusic_ytdlp_rollbacks_total",
        "Rückfälle auf die vorherige yt-dlp-Version",
        m::YTDLP_ROLLBACKS.get(),
    );

    if let Some(c) = cache {
        out.gauge("lemusic_cache_enabled", "Audio-Cache aktiv", c.enabled);
        out.gauge("lemusic_cache_entries", "Titel im Cache", c.entries);
        out.gauge("lemusic_cache_bytes", "Belegter Platz im Cache", c.bytes);
        out.gauge(
            "lemusic_cache_max_bytes",
            "Höchstgröße des Caches",
            c.max_bytes,
        );
        out.counter("lemusic_cache_hits_total", "Aus dem Cache gespielt", c.hits);
        out.counter("lemusic_cache_misses_total", "Neu geladen", c.misses);
    }

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out.0,
    )
        .into_response()
}

/// Kleiner Schreiber für das Prometheus-Textformat.
#[derive(Default)]
struct Metrics(String);

trait MetricValue {
    fn render(&self) -> String;
}

impl MetricValue for bool {
    fn render(&self) -> String {
        u8::from(*self).to_string()
    }
}

impl MetricValue for u64 {
    fn render(&self) -> String {
        self.to_string()
    }
}

impl MetricValue for usize {
    fn render(&self) -> String {
        self.to_string()
    }
}

impl MetricValue for f64 {
    fn render(&self) -> String {
        format!("{self}")
    }
}

impl Metrics {
    fn header(&mut self, name: &str, help: &str, kind: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}");
        let _ = writeln!(self.0, "# TYPE {name} {kind}");
    }

    fn gauge(&mut self, name: &str, help: &str, value: impl MetricValue) {
        self.header(name, help, "gauge");
        let _ = writeln!(self.0, "{name} {}", value.render());
    }

    fn counter(&mut self, name: &str, help: &str, value: u64) {
        self.header(name, help, "counter");
        let _ = writeln!(self.0, "{name} {value}");
    }

    fn summary(&mut self, name: &str, help: &str, sum: f64, count: u64) {
        self.header(name, help, "summary");
        let _ = writeln!(self.0, "{name}_sum {}", sum.render());
        let _ = writeln!(self.0, "{name}_count {count}");
    }
}

/// Belegter Arbeitsspeicher (Linux, aus `/proc/self/status`).
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lemusic_core::TrackMeta;
    use lemusic_player::{NowPlaying, QueueEntry, RepeatMode};

    use super::*;

    fn snapshot(playing: Option<(u64, u64)>, connected: bool) -> Snapshot {
        Snapshot {
            guild_id: 1,
            voice_channel: connected.then_some(5),
            text_channel: None,
            now: playing.map(|(id, pos)| NowPlaying {
                entry: QueueEntry {
                    id,
                    meta: Arc::new(TrackMeta {
                        title: "T".into(),
                        author: String::new(),
                        url: "https://youtu.be/x".into(),
                        duration: None,
                        thumbnail: None,
                    }),
                    requester: 0,
                    autoplay: false,
                    guest: false,
                },
                position: Duration::from_secs(pos),
                paused: false,
                ready: true,
            }),
            queue: Vec::new(),
            repeat: RepeatMode::Off,
            volume: 100,
        }
    }

    #[test]
    fn voice_needs_progress_while_playing() {
        let mut last = None;
        let t0 = Instant::now();
        // Nichts läuft → in Ordnung.
        assert!(voice_ok(&mut last, &snapshot(None, false), t0));
        // Läuft, aber nicht verbunden → gestört.
        assert!(!voice_ok(&mut last, &snapshot(Some((1, 10)), false), t0));
        // Läuft und bewegt sich.
        assert!(voice_ok(&mut last, &snapshot(Some((1, 10)), true), t0));
        assert!(voice_ok(
            &mut last,
            &snapshot(Some((1, 10)), true),
            t0 + Duration::from_secs(30)
        ));
        // Steht über eine Minute → gestört.
        assert!(!voice_ok(
            &mut last,
            &snapshot(Some((1, 10)), true),
            t0 + VOICE_STALL + Duration::from_secs(1)
        ));
        // Bewegt sich wieder → in Ordnung.
        assert!(voice_ok(
            &mut last,
            &snapshot(Some((1, 75)), true),
            t0 + Duration::from_secs(80)
        ));
    }

    #[test]
    fn renders_prometheus_text() {
        let mut m = Metrics::default();
        m.gauge("a_up", "Hilfe", true);
        m.counter("a_total", "Zähler", 3);
        m.summary("a_seconds", "Dauer", 1.5, 2);
        assert_eq!(
            m.0,
            "# HELP a_up Hilfe\n# TYPE a_up gauge\na_up 1\n\
             # HELP a_total Zähler\n# TYPE a_total counter\na_total 3\n\
             # HELP a_seconds Dauer\n# TYPE a_seconds summary\na_seconds_sum 1.5\na_seconds_count 2\n"
        );
    }
}
