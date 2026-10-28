//! Laufzeitmessung des Audio-Takts.
//!
//! songbird sendet alle 20 ms ein Audiopaket. Zwei Dinge können das stören:
//! - Der Mixer braucht pro Takt zu lange (Rechenzeit).
//! - Das System weckt Threads zu spät (CPU-Konkurrenz, VM-Scheduling).
//!
//! Beides wird hier gemessen und alle 60 s zusammengefasst; Auffälliges als
//! Warnung, sonst auf Debug-Level.

use std::{
    thread,
    time::{Duration, Instant},
};

use tracing::{debug, warn};

const TICK: Duration = Duration::from_millis(20);
const REPORT_EVERY: Duration = Duration::from_secs(60);
/// Ab dieser Verspätung eines Weckers ist ein Aussetzer wahrscheinlich.
const LATE_WARN: Duration = Duration::from_millis(15);
/// Ab dieser Rechenzeit pro Takt wird es knapp.
const COMPUTE_WARN: Duration = Duration::from_millis(10);

/// Startet den Mess-Thread (läuft für die Lebensdauer des Prozesses).
///
/// Muss im Tokio-Kontext aufgerufen werden: songbird startet seinen
/// Scheduler beim ersten Zugriff per `tokio::spawn`.
pub fn spawn() {
    let scheduler = songbird::driver::get_default_scheduler();
    let spawned = thread::Builder::new()
        .name("audio-diagnostics".into())
        .spawn(move || run(scheduler));
    if let Err(e) = spawned {
        warn!(error = %e, "Audio-Diagnose nicht startbar");
    }
}

fn run(scheduler: &'static songbird::driver::Scheduler) {
    let mut window_start = Instant::now();
    let mut worst_late = Duration::ZERO;
    let mut late_count = 0u32;
    let mut worst_compute = Duration::ZERO;
    let mut samples = 0u32;

    loop {
        let before = Instant::now();
        thread::sleep(TICK);
        let late = before.elapsed().saturating_sub(TICK);
        worst_late = worst_late.max(late);
        if late > LATE_WARN {
            late_count += 1;
        }

        if let Ok(stats) = scheduler.worker_thread_stats_blocking() {
            for block in stats {
                worst_compute =
                    worst_compute.max(Duration::from_nanos(block.last_compute_cost_ns()));
            }
        }
        samples += 1;

        if window_start.elapsed() >= REPORT_EVERY {
            // Rechenzeit > 0 heißt: In diesem Zeitraum wurde Audio gemischt.
            let playing = !worst_compute.is_zero();
            if playing && (late_count > 0 || worst_compute > COMPUTE_WARN) {
                warn!(
                    verspaetete_wecker = late_count,
                    max_verspaetung_ms = worst_late.as_millis() as u64,
                    max_rechenzeit_ms = worst_compute.as_secs_f64() * 1000.0,
                    messungen = samples,
                    "Audio-Takt gestört – mögliche Aussetzer (CPU-Last auf dem Host)"
                );
            } else {
                debug!(
                    max_verspaetung_ms = worst_late.as_millis() as u64,
                    max_rechenzeit_ms = worst_compute.as_secs_f64() * 1000.0,
                    spielt = playing,
                    "Audio-Takt ok"
                );
            }
            window_start = Instant::now();
            worst_late = Duration::ZERO;
            late_count = 0;
            worst_compute = Duration::ZERO;
            samples = 0;
        }
    }
}
