//! Zähler für `/metrics` (Prometheus-Format) – prozessweit, ohne Abhängigkeit.
//!
//! Alles sind einfache Atomics: Zählen kostet praktisch nichts und kann den
//! Audio-Takt nie blockieren.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Ein monoton steigender Zähler.
pub struct Counter(AtomicU64);

impl Counter {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn inc(&self) {
        self.add(1);
    }

    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Summe und Anzahl von Dauern (Prometheus „summary“ ohne Quantile).
pub struct DurationSum {
    micros: AtomicU64,
    count: AtomicU64,
}

impl DurationSum {
    const fn new() -> Self {
        Self {
            micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, d: Duration) {
        let micros = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn sum_secs(&self) -> f64 {
        self.micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

/// Gestartete Tracks (ohne Wiederholungen nach Fehlern).
pub static TRACKS_STARTED: Counter = Counter::new();
/// Normal zu Ende gespielte Tracks.
pub static TRACKS_FINISHED: Counter = Counter::new();
/// Endgültig fehlgeschlagene und übersprungene Tracks.
pub static TRACKS_FAILED: Counter = Counter::new();
/// Wiederholungen nach einem Track-Fehler.
pub static TRACK_RETRIES: Counter = Counter::new();
/// Streams, deren Position hing (Watchdog).
pub static STREAM_STALLS: Counter = Counter::new();
/// Zeit vom Start eines Tracks, bis er abspielbereit war.
pub static TRACK_LOAD: DurationSum = DurationSum::new();
/// Verspätete Wecker des Audio-Takts (> 15 ms) – mögliche Aussetzer.
pub static AUDIO_LATE_WAKEUPS: Counter = Counter::new();
/// Gemessene 20-ms-Takte der Audio-Diagnose.
pub static AUDIO_TICKS: Counter = Counter::new();
/// yt-dlp-Updates: Läufe, Fehlschläge und Rückfälle auf die alte Version.
pub static YTDLP_UPDATES: Counter = Counter::new();
pub static YTDLP_UPDATE_FAILURES: Counter = Counter::new();
pub static YTDLP_ROLLBACKS: Counter = Counter::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_sums() {
        let c = Counter::new();
        c.inc();
        c.add(2);
        assert_eq!(c.get(), 3);
        let d = DurationSum::new();
        d.observe(Duration::from_millis(1500));
        d.observe(Duration::from_millis(500));
        assert_eq!(d.count(), 2);
        assert!((d.sum_secs() - 2.0).abs() < 1e-9);
    }
}
