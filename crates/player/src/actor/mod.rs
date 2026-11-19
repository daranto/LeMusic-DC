//! Der Player eines Servers als Actor.
//!
//! Der gesamte Zustand gehört einem einzelnen Task; Befehle, songbird-Events
//! und ein Sekundentakt kommen als Nachrichten an. Dadurch gibt es keine
//! Wettläufe zwischen Skip, Track-Ende und Fehlern.
//!
//! songbird spielt nur den aktuellen Track und hält den nächsten pausiert und
//! vorbereitet bereit. Die Warteschlange selbst verwaltet der Actor.

use std::{
    collections::VecDeque,
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use lemusic_core::{GuildSettings, TrackMeta};
use serde::{Deserialize, Serialize};
use songbird::{
    Call, CoreEvent, Event, EventContext, EventHandler, TrackEvent,
    events::context_data::DisconnectReason,
    model::CloseCode,
    tracks::{PlayMode, ReadyState, TrackHandle},
};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    build_track,
    manager::Shared,
    metrics,
    model::{
        AutoJoinOutcome, EnqueueOutcome, EnqueueRequest, MAX_GUEST_QUEUED, MAX_QUEUED_REQUESTS,
        NowPlaying, PlayerEvent, QueueEdit, QueueEntry, RepeatMode, Snapshot,
    },
    resolve::permanent_reason,
};

/// Zeit, die wir Discord für den Voice-Handshake geben.
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Versuche für einen Beitritt (kurz nach dem Verlassen scheitert der erste
/// gelegentlich).
const JOIN_ATTEMPTS: u32 = 3;
/// Wiederholungen eines fehlgeschlagenen Tracks, bevor er übersprungen wird.
const MAX_RETRIES: u8 = 2;
/// Bewegt sich die Position so lange nicht, gilt der Stream als hängend.
const STALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Endet ein Track mehr als so viel vor seiner Länge, war es ein Abbruch.
const EARLY_END_TOLERANCE: Duration = Duration::from_secs(10);
/// Versuche, nach einem Verbindungsabbruch wieder beizutreten.
const REJOIN_DELAYS: [u64; 4] = [2, 5, 10, 30];
/// Zustand spätestens so oft speichern (Position).
const SAVE_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) type Reply<T> = oneshot::Sender<Result<T>>;

pub(crate) enum Msg {
    Enqueue(EnqueueRequest, Reply<EnqueueOutcome>),
    /// Sprachkanal betreten, ohne etwas abzuspielen (Sprachkanal, Textkanal).
    /// Antwort: Name der Autoplaylist, falls sie gestartet wurde.
    Join(u64, Option<u64>, Reply<Option<String>>),
    AutoJoin(u64, Reply<AutoJoinOutcome>),
    Skip(Reply<Option<QueueEntry>>),
    SkipTo(usize, Reply<()>),
    Pause(Reply<()>),
    Resume(Reply<()>),
    Stop(Reply<()>),
    Seek(Duration, Reply<()>),
    Volume(u16, Reply<()>),
    Repeat(RepeatMode, Reply<()>),
    Shuffle(Reply<()>),
    Remove(usize, Reply<QueueEntry>),
    Move(usize, usize, Reply<QueueEntry>),
    Clear(Reply<usize>),
    Edit(QueueEdit, Reply<usize>),
    Snapshot(oneshot::Sender<Snapshot>),
    Restore(Box<SavedState>),
    /// Zustand sichern und Kanal verlassen, Warteschlange bleibt gespeichert.
    Shutdown(oneshot::Sender<()>),
    // Interne Ereignisse:
    TrackEnded {
        uuid: Uuid,
        error: Option<String>,
    },
    Disconnected {
        kicked: bool,
    },
}

mod events;
mod playback;
mod queue;
mod state;
mod voice;

use queue::{apply_edit, enqueue_room, request_insert_index, take_entries};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedState {
    pub voice_channel: u64,
    pub text_channel: Option<u64>,
    pub current: Option<SavedCurrent>,
    pub queue: Vec<QueueEntry>,
    pub repeat: RepeatMode,
    pub volume: u16,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedCurrent {
    pub entry: QueueEntry,
    pub position: Duration,
}

pub(crate) fn state_key(guild_id: u64) -> String {
    format!("player:{guild_id}")
}

struct Playing {
    entry: QueueEntry,
    handle: TrackHandle,
    /// Startposition dieses Streams (Seek/Fortsetzen).
    start_at: Duration,
    /// Absolute Position im Track.
    position: Duration,
    last_progress: Instant,
    retries: u8,
    ready: bool,
    /// Startzeitpunkt, bis zur Abspielbereitschaft (für die Ladezeit-Metrik).
    loading_since: Option<Instant>,
}

pub(crate) struct Actor {
    guild_id: u64,
    shared: Arc<Shared>,
    tx: mpsc::WeakSender<Msg>,
    call: Option<Arc<Mutex<Call>>>,
    voice_channel: Option<u64>,
    text_channel: Option<u64>,
    queue: VecDeque<QueueEntry>,
    current: Option<Playing>,
    /// Vorbereiteter nächster Track (Entry-ID, pausierter Handle).
    prepared: Option<(u64, TrackHandle)>,
    repeat: RepeatMode,
    volume: u16,
    paused: bool,
    /// Automatisch pausiert, weil niemand mehr zuhört.
    auto_paused: bool,
    idle_since: Option<Instant>,
    alone_since: Option<Instant>,
    next_id: u64,
    dirty: bool,
    last_save: Instant,
    /// Zuletzt entfernte Einträge (alte Position) für „Rückgängig“.
    trash: Vec<(usize, QueueEntry)>,
}

impl Actor {
    pub(crate) fn new(guild_id: u64, shared: Arc<Shared>, tx: mpsc::WeakSender<Msg>) -> Self {
        let volume = shared.settings.current().default_volume;
        Self {
            guild_id,
            shared,
            tx,
            call: None,
            voice_channel: None,
            text_channel: None,
            queue: VecDeque::new(),
            current: None,
            prepared: None,
            repeat: RepeatMode::Off,
            volume,
            paused: false,
            auto_paused: false,
            idle_since: None,
            alone_since: None,
            next_id: 1,
            dirty: false,
            last_save: Instant::now(),
            trash: Vec::new(),
        }
    }

    pub(crate) async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    if self.handle(msg).await {
                        break;
                    }
                }
                _ = tick.tick() => self.on_tick().await,
            }
            if self.dirty {
                self.emit(PlayerEvent::Changed {
                    guild_id: self.guild_id,
                });
            }
        }
        debug!(guild_id = self.guild_id, "Player beendet");
    }

    /// Verarbeitet eine Nachricht. `true` = Actor beenden.
    async fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Enqueue(req, reply) => {
                let r = self.enqueue(req).await;
                let _ = reply.send(r);
            }
            Msg::Join(channel, text_channel, reply) => {
                let r = self.join(channel, text_channel).await;
                let _ = reply.send(r);
            }
            Msg::AutoJoin(channel, reply) => {
                let r = self.auto_join(channel).await;
                let _ = reply.send(r);
            }
            Msg::Skip(reply) => {
                let skipped = self.current.as_ref().map(|c| c.entry.clone());
                let r = match skipped {
                    Some(_) => self.skip_current().await.map(|()| skipped),
                    None => Err(anyhow!("Es läuft gerade nichts")),
                };
                let _ = reply.send(r);
            }
            Msg::SkipTo(index, reply) => {
                let r = self.skip_to(index).await;
                let _ = reply.send(r);
            }
            Msg::Pause(reply) => {
                let r = self.set_paused(true);
                self.auto_paused = false;
                let _ = reply.send(r);
            }
            Msg::Resume(reply) => {
                let r = self.set_paused(false);
                self.auto_paused = false;
                let _ = reply.send(r);
            }
            Msg::Stop(reply) => {
                let r = self.stop("Gestoppt").await;
                let _ = reply.send(r);
            }
            Msg::Seek(pos, reply) => {
                let r = self.seek(pos).await;
                let _ = reply.send(r);
            }
            Msg::Volume(v, reply) => {
                let r = self.set_volume(v);
                let _ = reply.send(r);
            }
            Msg::Repeat(mode, reply) => {
                self.repeat = mode;
                self.dirty = true;
                self.prepare_next().await;
                let _ = reply.send(Ok(()));
            }
            Msg::Shuffle(reply) => {
                let r = if self.queue.len() < 2 {
                    Err(anyhow!("Zu wenige Tracks zum Mischen"))
                } else {
                    let mut items: Vec<_> = self.queue.drain(..).collect();
                    fastrand::shuffle(&mut items);
                    self.queue = items.into();
                    self.dirty = true;
                    self.prepare_next().await;
                    Ok(())
                };
                let _ = reply.send(r);
            }
            Msg::Remove(index, reply) => {
                let r = self
                    .queue
                    .remove(index)
                    .ok_or_else(|| anyhow!("Kein Track an Position {}", index + 1));
                if let Ok(entry) = &r {
                    self.trash = vec![(index, entry.clone())];
                    self.dirty = true;
                    self.prepare_next().await;
                }
                let _ = reply.send(r);
            }
            Msg::Move(from, to, reply) => {
                let r = match self.queue.remove(from) {
                    Some(entry) => {
                        let to = to.min(self.queue.len());
                        self.queue.insert(to, entry.clone());
                        self.dirty = true;
                        self.prepare_next().await;
                        Ok(entry)
                    }
                    None => Err(anyhow!("Kein Track an Position {}", from + 1)),
                };
                let _ = reply.send(r);
            }
            Msg::Clear(reply) => {
                let n = take_entries(&mut self.queue, &mut self.trash, |_| true);
                self.drop_prepared();
                self.dirty = true;
                let _ = reply.send(Ok(n));
            }
            Msg::Edit(edit, reply) => {
                let r = apply_edit(&mut self.queue, &mut self.trash, edit);
                if r.is_ok() {
                    self.dirty = true;
                    self.prepare_next().await;
                }
                let _ = reply.send(r);
            }
            Msg::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Msg::Restore(saved) => self.restore(*saved).await,
            Msg::Shutdown(reply) => {
                self.save().await;
                self.leave_voice().await;
                let _ = reply.send(());
                return true;
            }
            Msg::TrackEnded { uuid, error } => self.on_track_ended(uuid, error).await,
            Msg::Disconnected { kicked } => self.on_disconnected(kicked).await,
        }
        false
    }
}

/// Leitet Track-Ende/-Fehler an den Actor weiter.
struct TrackWatcher {
    tx: mpsc::WeakSender<Msg>,
    uuid: Uuid,
}

#[async_trait::async_trait]
impl EventHandler for TrackWatcher {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let EventContext::Track(states) = ctx else {
            return None;
        };
        let error = states.first().and_then(|(state, _)| match &state.playing {
            PlayMode::Errored(e) => Some(e.to_string()),
            _ => None,
        });
        if let Some(tx) = self.tx.upgrade() {
            let _ = tx
                .send(Msg::TrackEnded {
                    uuid: self.uuid,
                    error,
                })
                .await;
        }
        None
    }
}

/// Meldet Abbrüche der Voice-Verbindung an den Actor.
struct DriverWatcher {
    tx: mpsc::WeakSender<Msg>,
}

#[async_trait::async_trait]
impl EventHandler for DriverWatcher {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let EventContext::DriverDisconnect(data) = ctx else {
            return None;
        };
        let kicked = match data.reason {
            // Selbst verlassen – kein Handlungsbedarf.
            Some(DisconnectReason::Requested) => return None,
            Some(DisconnectReason::WsClosed(Some(CloseCode::Disconnected))) => true,
            _ => false,
        };
        if let Some(tx) = self.tx.upgrade() {
            let _ = tx.send(Msg::Disconnected { kicked }).await;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::resolve::permanent_reason;

    #[test]
    fn recognizes_permanent_errors() {
        let age = "Stream ist leer – [youtube] OT2IKn6ycbA: Sign in to confirm your age. This video may be inappropriate";
        assert_eq!(
            permanent_reason(age),
            Some("altersbeschränkt (dafür sind YouTube-Cookies nötig)")
        );
        assert_eq!(
            permanent_reason("[youtube] Zm7lGk6bnkM: This video is unavailable"),
            Some("nicht verfügbar")
        );
        assert_eq!(permanent_reason("Stream liefert keine Daten"), None);
        assert_eq!(permanent_reason("Stream endete vorzeitig bei 1:23"), None);
    }
}
