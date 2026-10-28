//! Verwaltet die Player aller Server.
//!
//! Stürzt ein Player-Task ab (Panic), wird er beim nächsten Zugriff neu
//! gestartet und aus dem gespeicherten Zustand wiederhergestellt.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use lemusic_core::{Db, SettingsStore};
use songbird::Songbird;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::{
    actor::{Actor, Msg, Reply, SavedState, state_key},
    model::{
        EnqueueOutcome, EnqueueRequest, PlayerEvent, QueueEntry, RepeatMode, Snapshot,
        VoicePresence,
    },
};

/// Länge der Nachrichten-Warteschlange pro Player.
const MAILBOX: usize = 64;
/// So lange wartet ein Befehl höchstens auf den Player.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Shared {
    pub songbird: Arc<Songbird>,
    pub db: Db,
    pub settings: SettingsStore,
    pub events: broadcast::Sender<PlayerEvent>,
    pub presence: Arc<dyn VoicePresence>,
}

#[derive(Clone)]
pub struct PlayerManager {
    shared: Arc<Shared>,
    players: Arc<std::sync::Mutex<HashMap<u64, mpsc::Sender<Msg>>>>,
}

impl PlayerManager {
    pub fn new(
        songbird: Arc<Songbird>,
        db: Db,
        settings: SettingsStore,
        presence: Arc<dyn VoicePresence>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            shared: Arc::new(Shared {
                songbird,
                db,
                settings,
                events,
                presence,
            }),
            players: Arc::default(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.shared.events.subscribe()
    }

    /// Liefert den Player eines Servers; startet ihn bei Bedarf (neu).
    pub fn player(&self, guild_id: u64) -> Player {
        let tx = {
            let mut players = match self.players.lock() {
                Ok(p) => p,
                Err(poisoned) => poisoned.into_inner(),
            };
            match players.get(&guild_id) {
                Some(tx) if !tx.is_closed() => tx.clone(),
                existing => {
                    let crashed = existing.is_some();
                    let tx = self.spawn(guild_id);
                    players.insert(guild_id, tx.clone());
                    if crashed {
                        error!(guild_id, "Player war abgestürzt – neu gestartet");
                        self.spawn_restore(guild_id, tx.clone());
                    }
                    tx
                }
            }
        };
        Player { tx }
    }

    fn spawn(&self, guild_id: u64) -> mpsc::Sender<Msg> {
        let (tx, rx) = mpsc::channel(MAILBOX);
        let actor = Actor::new(guild_id, self.shared.clone(), tx.downgrade());
        tokio::spawn(actor.run(rx));
        tx
    }

    fn spawn_restore(&self, guild_id: u64, tx: mpsc::Sender<Msg>) {
        let db = self.shared.db.clone();
        tokio::spawn(async move {
            match db.get_json::<SavedState>(&state_key(guild_id)).await {
                Ok(Some(saved)) => {
                    let _ = tx.send(Msg::Restore(Box::new(saved))).await;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, "Gespeicherter Player-Zustand unlesbar"),
            }
        });
    }

    /// Stellt nach dem Start alle gespeicherten Player wieder her.
    pub async fn restore_all(&self) {
        let keys = match self.shared.db.keys_with_prefix("player:").await {
            Ok(k) => k,
            Err(e) => {
                warn!(error = %e, "Gespeicherte Player nicht lesbar");
                return;
            }
        };
        for key in keys {
            let Some(guild_id) = key.strip_prefix("player:").and_then(|g| g.parse().ok()) else {
                continue;
            };
            info!(guild_id, "Stelle Wiedergabe wieder her");
            let Player { tx } = self.player(guild_id);
            self.spawn_restore(guild_id, tx);
        }
    }

    /// Sichert alle Player und verlässt die Sprachkanäle (Warteschlangen
    /// bleiben für den nächsten Start gespeichert).
    pub async fn shutdown(&self) {
        let senders: Vec<_> = match self.players.lock() {
            Ok(p) => p.values().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().values().cloned().collect(),
        };
        for tx in senders {
            let (reply, rx) = oneshot::channel();
            if tx.send(Msg::Shutdown(reply)).await.is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
            }
        }
    }

    /// Momentaufnahmen aller aktiven Player.
    pub async fn snapshots(&self) -> Vec<Snapshot> {
        let guilds: Vec<u64> = match self.players.lock() {
            Ok(p) => p.keys().copied().collect(),
            Err(poisoned) => poisoned.into_inner().keys().copied().collect(),
        };
        let mut out = Vec::new();
        for guild_id in guilds {
            if let Ok(s) = self.player(guild_id).snapshot().await {
                out.push(s);
            }
        }
        out
    }
}

/// Handle auf den Player eines Servers.
#[derive(Clone)]
pub struct Player {
    tx: mpsc::Sender<Msg>,
}

impl Player {
    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Msg) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(make(reply))
            .await
            .map_err(|_| anyhow!("Player ist nicht erreichbar"))?;
        tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow!("Player antwortet nicht"))?
            .map_err(|_| anyhow!("Player wurde beendet"))?
    }

    pub async fn enqueue(&self, req: EnqueueRequest) -> Result<EnqueueOutcome> {
        self.request(|r| Msg::Enqueue(req, r)).await
    }
    /// Überspringt den aktuellen Track; liefert ihn zurück.
    pub async fn skip(&self) -> Result<Option<QueueEntry>> {
        self.request(Msg::Skip).await
    }
    /// Springt zu Position `index` (0-basiert) der Warteschlange.
    pub async fn skip_to(&self, index: usize) -> Result<()> {
        self.request(|r| Msg::SkipTo(index, r)).await
    }
    pub async fn pause(&self) -> Result<()> {
        self.request(Msg::Pause).await
    }
    pub async fn resume(&self) -> Result<()> {
        self.request(Msg::Resume).await
    }
    pub async fn stop(&self) -> Result<()> {
        self.request(Msg::Stop).await
    }
    pub async fn seek(&self, position: Duration) -> Result<()> {
        self.request(|r| Msg::Seek(position, r)).await
    }
    pub async fn set_volume(&self, percent: u16) -> Result<()> {
        self.request(|r| Msg::Volume(percent, r)).await
    }
    pub async fn set_repeat(&self, mode: RepeatMode) -> Result<()> {
        self.request(|r| Msg::Repeat(mode, r)).await
    }
    pub async fn shuffle(&self) -> Result<()> {
        self.request(Msg::Shuffle).await
    }
    /// Entfernt Position `index` (0-basiert).
    pub async fn remove(&self, index: usize) -> Result<QueueEntry> {
        self.request(|r| Msg::Remove(index, r)).await
    }
    /// Verschiebt von `from` nach `to` (0-basiert).
    pub async fn move_entry(&self, from: usize, to: usize) -> Result<QueueEntry> {
        self.request(|r| Msg::Move(from, to, r)).await
    }
    pub async fn clear(&self) -> Result<usize> {
        self.request(Msg::Clear).await
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Snapshot(reply))
            .await
            .map_err(|_| anyhow!("Player ist nicht erreichbar"))?;
        tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow!("Player antwortet nicht"))?
            .map_err(|_| anyhow!("Player wurde beendet"))
    }
}
