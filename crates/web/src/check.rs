//! Verfügbarkeitsprüfung von Playlists im Hintergrund.
//!
//! Prüft jeden Titel einzeln mit yt-dlp (gelöscht, privat, gesperrt …).
//! Läuft immer nur eine Prüfung gleichzeitig und mit wenig Parallelität,
//! damit YouTube den Server nicht drosselt.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use lemusic_core::TrackMeta;
use lemusic_player::{Availability, check_available};
use serde::Serialize;
use tokio::{sync::Semaphore, task::JoinSet};
use tracing::info;

/// Gleichzeitige yt-dlp-Prüfungen.
const PARALLEL: usize = 2;
/// Kurze Pause zwischen zwei Prüfungen pro Slot.
const PAUSE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Serialize)]
pub struct GoneTrack {
    pub url: String,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CheckStatus {
    pub running: bool,
    pub total: usize,
    pub done: usize,
    /// Nicht mehr abspielbar.
    pub gone: Vec<GoneTrack>,
    /// Konnte nicht geprüft werden (Netzwerk, Drosselung).
    pub unknown: usize,
    pub cancelled: bool,
}

struct Job {
    status: CheckStatus,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Checks {
    jobs: Mutex<HashMap<String, Job>>,
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl Checks {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        match self.jobs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn status(&self, name: &str) -> Option<CheckStatus> {
        self.lock().get(&key(name)).map(|j| j.status.clone())
    }

    pub fn cancel(&self, name: &str) {
        if let Some(job) = self.lock().get(&key(name)) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Vergisst das Ergebnis (z. B. nach Umbenennen oder Löschen).
    pub fn forget(&self, name: &str) {
        if let Some(job) = self.lock().remove(&key(name)) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Startet eine Prüfung. Fehler, wenn schon eine läuft.
    pub fn start(self: &Arc<Self>, name: &str, tracks: Vec<TrackMeta>) -> Result<(), &'static str> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.lock();
            if jobs.values().any(|j| j.status.running) {
                return Err("Es läuft schon eine Prüfung – bitte warten oder abbrechen");
            }
            jobs.insert(
                key(name),
                Job {
                    status: CheckStatus {
                        running: true,
                        total: tracks.len(),
                        ..CheckStatus::default()
                    },
                    cancel: cancel.clone(),
                },
            );
        }
        let this = self.clone();
        let name = name.trim().to_string();
        tokio::spawn(async move {
            info!(playlist = %name, tracks = tracks.len(), "Verfügbarkeitsprüfung gestartet");
            let slots = Arc::new(Semaphore::new(PARALLEL));
            let mut set = JoinSet::new();
            for track in tracks {
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    break;
                };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let this = this.clone();
                let name = name.clone();
                set.spawn(async move {
                    let result = check_available(&track.url).await;
                    this.record(&name, &track, result);
                    tokio::time::sleep(PAUSE).await;
                    drop(permit);
                });
            }
            while set.join_next().await.is_some() {}
            let summary = {
                let mut jobs = this.lock();
                jobs.get_mut(&key(&name)).map(|job| {
                    job.status.running = false;
                    job.status.cancelled = cancel.load(Ordering::Relaxed);
                    (job.status.gone.len(), job.status.unknown)
                })
            };
            if let Some((gone, unknown)) = summary {
                info!(playlist = %name, gone, unknown, "Verfügbarkeitsprüfung beendet");
            }
        });
        Ok(())
    }

    fn record(&self, name: &str, track: &TrackMeta, result: Availability) {
        let mut jobs = self.lock();
        let Some(job) = jobs.get_mut(&key(name)) else {
            return;
        };
        job.status.done += 1;
        match result {
            Availability::Available => {}
            Availability::Gone(reason) => job.status.gone.push(GoneTrack {
                url: track.url.clone(),
                title: track.title.clone(),
                reason,
            }),
            Availability::Unknown(_) => job.status.unknown += 1,
        }
    }
}
