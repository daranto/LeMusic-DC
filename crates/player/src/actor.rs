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

/// Wo ein Wunsch eingereiht wird: vor dem ersten Autoplaylist-Eintrag,
/// sonst ans Ende.
fn request_insert_index(queue: &VecDeque<QueueEntry>) -> usize {
    queue.iter().position(|e| e.autoplay).unwrap_or(queue.len())
}

/// Wie viele Tracks noch eingereiht werden dürfen: Wünsche insgesamt und –
/// bei Gästen – zusätzlich deren eigenes Kontingent. Autoplaylist-Einträge
/// zählen nicht mit.
fn enqueue_room(queue: &VecDeque<QueueEntry>, guest: bool) -> usize {
    let requests = queue.iter().filter(|e| !e.autoplay).count();
    let room = MAX_QUEUED_REQUESTS.saturating_sub(requests);
    if !guest {
        return room;
    }
    let guests = queue.iter().filter(|e| e.guest).count();
    room.min(MAX_GUEST_QUEUED.saturating_sub(guests))
}

/// Höchstens so viele entfernte Einträge für „Rückgängig“ merken.
const MAX_TRASH: usize = 2000;

/// Entfernt alle Einträge, für die `remove` gilt, und merkt sie sich
/// (mit alter Position) für „Rückgängig“.
fn take_entries(
    queue: &mut VecDeque<QueueEntry>,
    trash: &mut Vec<(usize, QueueEntry)>,
    mut remove: impl FnMut(&QueueEntry) -> bool,
) -> usize {
    let mut removed = Vec::new();
    let mut kept = VecDeque::with_capacity(queue.len());
    for (i, e) in queue.drain(..).enumerate() {
        if remove(&e) {
            removed.push((i, e));
        } else {
            kept.push_back(e);
        }
    }
    *queue = kept;
    let n = removed.len();
    if n > 0 {
        removed.truncate(MAX_TRASH);
        *trash = removed;
    }
    n
}

/// Wendet eine Bearbeitung an; liefert die Zahl der betroffenen Einträge.
fn apply_edit(
    queue: &mut VecDeque<QueueEntry>,
    trash: &mut Vec<(usize, QueueEntry)>,
    edit: QueueEdit,
) -> Result<usize> {
    use std::collections::HashSet;
    match edit {
        QueueEdit::Remove { ids } => {
            let ids: HashSet<u64> = ids.into_iter().collect();
            let n = take_entries(queue, trash, |e| ids.contains(&e.id));
            if n == 0 {
                bail!("Die Titel sind nicht mehr in der Warteschlange");
            }
            Ok(n)
        }
        QueueEdit::Move { ids, before, front } => {
            let set: HashSet<u64> = ids.into_iter().collect();
            if before.is_some_and(|b| set.contains(&b)) {
                bail!("Ungültiges Ziel");
            }
            if !queue.iter().any(|e| set.contains(&e.id)) {
                bail!("Die Titel sind nicht mehr in der Warteschlange");
            }
            if let Some(b) = before
                && !front
                && !queue.iter().any(|e| e.id == b)
            {
                bail!("Das Ziel ist nicht mehr in der Warteschlange – bitte neu laden");
            }
            let (moved, mut rest): (VecDeque<_>, VecDeque<_>) =
                queue.drain(..).partition(|e| set.contains(&e.id));
            let at = match (front, before) {
                (true, _) => 0,
                (false, Some(b)) => rest.iter().position(|e| e.id == b).unwrap_or(rest.len()),
                (false, None) => rest.len(),
            };
            let n = moved.len();
            for (offset, e) in moved.into_iter().enumerate() {
                rest.insert(at + offset, e);
            }
            *queue = rest;
            Ok(n)
        }
        QueueEdit::Restore => {
            if trash.is_empty() {
                bail!("Nichts zum Wiederherstellen");
            }
            let n = trash.len();
            for (i, e) in trash.drain(..) {
                let i = i.min(queue.len());
                queue.insert(i, e);
            }
            Ok(n)
        }
        QueueEdit::Dedupe => {
            let mut seen = HashSet::new();
            Ok(take_entries(queue, trash, |e| !seen.insert(e.meta.key())))
        }
        QueueEdit::RemoveAutoplay => Ok(take_entries(queue, trash, |e| e.autoplay)),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc};

    use lemusic_core::TrackMeta;

    use super::{
        MAX_GUEST_QUEUED, MAX_QUEUED_REQUESTS, QueueEdit, QueueEntry, apply_edit, enqueue_room,
        request_insert_index,
    };
    use crate::resolve::permanent_reason;

    fn entry(id: u64, autoplay: bool) -> QueueEntry {
        QueueEntry {
            id,
            meta: Arc::new(TrackMeta {
                title: format!("T{id}"),
                author: String::new(),
                url: format!("https://youtu.be/{id}"),
                duration: None,
                thumbnail: None,
            }),
            requester: 0,
            autoplay,
            guest: false,
        }
    }

    #[test]
    fn enqueue_room_limits_requests_and_guests() {
        // Autoplaylist zählt nicht mit.
        let auto: VecDeque<_> = (0..3000).map(|i| entry(i, true)).collect();
        assert_eq!(enqueue_room(&auto, false), MAX_QUEUED_REQUESTS);
        assert_eq!(enqueue_room(&auto, true), MAX_GUEST_QUEUED);

        // Wünsche: Gesamtgrenze.
        let mut q: VecDeque<_> = (0..MAX_QUEUED_REQUESTS as u64 - 5)
            .map(|i| entry(i, false))
            .collect();
        assert_eq!(enqueue_room(&q, false), 5);
        assert_eq!(enqueue_room(&q, true), 5);

        // Gäste: eigenes Kontingent.
        q.clear();
        q.extend((0..MAX_GUEST_QUEUED as u64 - 2).map(|i| {
            let mut e = entry(i, false);
            e.guest = true;
            e
        }));
        assert_eq!(enqueue_room(&q, true), 2);
        assert_eq!(enqueue_room(&q, false), MAX_QUEUED_REQUESTS - q.len());
    }

    #[test]
    fn requests_go_before_autoplaylist() {
        // Nur Autoplaylist → Wunsch kommt als Nächstes.
        let only_auto: VecDeque<_> = [entry(1, true), entry(2, true)].into();
        assert_eq!(request_insert_index(&only_auto), 0);
        // Wünsche + Autoplaylist → hinter die Wünsche, vor die Autoplaylist.
        let mixed: VecDeque<_> = [entry(1, false), entry(2, false), entry(3, true)].into();
        assert_eq!(request_insert_index(&mixed), 2);
        // Keine Autoplaylist → ans Ende.
        let requests: VecDeque<_> = [entry(1, false)].into();
        assert_eq!(request_insert_index(&requests), 1);
        assert_eq!(request_insert_index(&VecDeque::new()), 0);
    }

    fn ids(q: &VecDeque<QueueEntry>) -> Vec<u64> {
        q.iter().map(|e| e.id).collect()
    }

    #[test]
    fn queue_edits_by_id() -> anyhow::Result<()> {
        let mut q: VecDeque<_> = (1..=5).map(|i| entry(i, i > 3)).collect();
        let mut trash = Vec::new();
        // 2 und 4 vor 1
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![4, 2],
                before: Some(1),
                front: false,
            },
        )?;
        assert_eq!(ids(&q), [2, 4, 1, 3, 5]);
        // 5 nach vorne, 2 ans Ende
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![5],
                before: None,
                front: true,
            },
        )?;
        apply_edit(
            &mut q,
            &mut trash,
            QueueEdit::Move {
                ids: vec![2],
                before: None,
                front: false,
            },
        )?;
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        // Ziel weg → Fehler, nichts verändert
        assert!(
            apply_edit(
                &mut q,
                &mut trash,
                QueueEdit::Move {
                    ids: vec![1],
                    before: Some(99),
                    front: false
                }
            )
            .is_err()
        );
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        // Entfernen + Rückgängig stellt Positionen wieder her
        assert_eq!(
            apply_edit(&mut q, &mut trash, QueueEdit::Remove { ids: vec![4, 3] })?,
            2
        );
        assert_eq!(ids(&q), [5, 1, 2]);
        apply_edit(&mut q, &mut trash, QueueEdit::Restore)?;
        assert_eq!(ids(&q), [5, 4, 1, 3, 2]);
        assert!(apply_edit(&mut q, &mut trash, QueueEdit::Restore).is_err());
        // Autoplaylist-Titel entfernen (4 und 5)
        assert_eq!(
            apply_edit(&mut q, &mut trash, QueueEdit::RemoveAutoplay)?,
            2
        );
        assert_eq!(ids(&q), [1, 3, 2]);
        Ok(())
    }

    #[test]
    fn dedupe_keeps_first() -> anyhow::Result<()> {
        let mut q: VecDeque<_> = [entry(1, false), entry(2, false), entry(1, true)]
            .into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                e.id = i as u64 + 10;
                e
            })
            .collect();
        let mut trash = Vec::new();
        assert_eq!(apply_edit(&mut q, &mut trash, QueueEdit::Dedupe)?, 1);
        assert_eq!(ids(&q), [10, 11]);
        Ok(())
    }

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

    // -----------------------------------------------------------------------
    // Sprachkanal
    // -----------------------------------------------------------------------

    async fn ensure_joined(&mut self, channel: u64) -> Result<Arc<Mutex<Call>>> {
        let channel_nz = NonZeroU64::new(channel).ok_or_else(|| anyhow!("Ungültiger Kanal"))?;
        if let Some(call) = &self.call {
            let current = call.lock().await.current_channel();
            if current.map(|c| c.0) == Some(channel_nz) {
                return Ok(call.clone());
            }
            if current.is_some() && self.current.is_some() {
                bail!("Ich spiele bereits in einem anderen Sprachkanal");
            }
        }

        let guild = NonZeroU64::new(self.guild_id).ok_or_else(|| anyhow!("Ungültiger Server"))?;
        // Direkt nach dem Verlassen räumt songbird die alte Verbindung noch ab
        // („request was cancelled/dropped“) – dann einmal kurz später erneut.
        let mut attempt = 0;
        let call = loop {
            attempt += 1;
            let result =
                tokio::time::timeout(JOIN_TIMEOUT, self.shared.songbird.join(guild, channel_nz))
                    .await
                    .context("Zeitüberschreitung beim Betreten des Sprachkanals")
                    .and_then(|r| r.context("Sprachkanal konnte nicht betreten werden"));
            match result {
                Ok(call) => break call,
                Err(e) if attempt < JOIN_ATTEMPTS => {
                    // Discord führt den Bot nach schnellem Verlassen/Beitreten
                    // evtl. noch im Kanal – ein erneuter Beitritt wäre für
                    // Discord keine Änderung und bliebe unbeantwortet. Daher
                    // erst ausdrücklich abmelden.
                    warn!(error = %e, attempt, "Beitritt fehlgeschlagen – melde ab und versuche erneut");
                    if let Err(e) = self.shared.songbird.leave(guild).await {
                        debug!(error = %e, "Abmelden fehlgeschlagen");
                    }
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                Err(e) => return Err(e),
            }
        };

        {
            let mut handler = call.lock().await;
            if let Err(e) = handler.deafen(true).await {
                warn!(error = %e, "Selbst-Taubschalten fehlgeschlagen");
            }
            handler.remove_all_global_events();
            handler.add_global_event(
                Event::Core(CoreEvent::DriverDisconnect),
                DriverWatcher {
                    tx: self.tx.clone(),
                },
            );
        }
        info!(guild_id = self.guild_id, channel, "Sprachkanal betreten");
        self.call = Some(call.clone());
        self.voice_channel = Some(channel);
        self.dirty = true;
        Ok(call)
    }

    async fn leave_voice(&mut self) {
        self.drop_prepared();
        if let Some(current) = self.current.take() {
            let _ = current.handle.stop();
            // Für den Fall, dass dies nur ein vorübergehendes Verlassen ist
            // (Shutdown), bleibt der Track in `queue`-Form im gespeicherten
            // Zustand erhalten.
        }
        // Nur verlassen, das Call-Objekt aber behalten: songbird nutzt es beim
        // nächsten Beitritt wieder. Ein `remove()` direkt vor einem neuen
        // Beitritt ließ diesen sonst dauerhaft ins Leere laufen.
        let call = self
            .call
            .take()
            .or_else(|| NonZeroU64::new(self.guild_id).and_then(|g| self.shared.songbird.get(g)));
        if let Some(call) = call {
            let mut handler = call.lock().await;
            handler.remove_all_global_events();
            handler.stop();
            if let Err(e) = handler.leave().await {
                debug!(error = %e, "Verlassen des Sprachkanals fehlgeschlagen");
            }
        }
    }

    async fn on_disconnected(&mut self, kicked: bool) {
        if self.call.is_none() {
            return;
        }
        if kicked {
            info!(guild_id = self.guild_id, "Aus dem Sprachkanal entfernt");
            let _ = self
                .stop_with_reason("Ich wurde aus dem Sprachkanal entfernt")
                .await;
            return;
        }

        let Some(channel) = self.voice_channel else {
            return;
        };
        warn!(
            guild_id = self.guild_id,
            "Voice-Verbindung verloren – verbinde neu"
        );
        let resume_at = self.current.as_ref().map(|c| c.position);
        if let Some(current) = &self.current {
            let _ = current.handle.stop();
        }
        self.drop_prepared();
        self.call = None;

        for (attempt, delay) in REJOIN_DELAYS.iter().enumerate() {
            tokio::time::sleep(Duration::from_secs(*delay)).await;
            match self.ensure_joined(channel).await {
                Ok(_) => {
                    info!(
                        guild_id = self.guild_id,
                        attempt = attempt + 1,
                        "Wieder verbunden"
                    );
                    if let (Some(current), Some(pos)) = (self.current.take(), resume_at)
                        && let Err(e) = self.start(current.entry, pos, current.retries, false).await
                    {
                        warn!(error = %e, "Fortsetzen nach Reconnect fehlgeschlagen");
                        self.advance(false).await;
                    }
                    return;
                }
                Err(e) => warn!(attempt = attempt + 1, error = %e, "Reconnect fehlgeschlagen"),
            }
        }
        let _ = self
            .stop_with_reason("Verbindung zum Sprachkanal verloren")
            .await;
    }

    // -----------------------------------------------------------------------
    // Wiedergabe
    // -----------------------------------------------------------------------

    async fn enqueue(&mut self, req: EnqueueRequest) -> Result<EnqueueOutcome> {
        if req.tracks.is_empty() {
            bail!("Keine Tracks");
        }
        let room = enqueue_room(&self.queue, req.guest);
        if room == 0 {
            if req.guest {
                bail!(
                    "Es warten schon {MAX_GUEST_QUEUED} Gast-Wünsche – bitte warten, bis ein paar gespielt wurden"
                );
            }
            bail!("Die Warteschlange ist voll (höchstens {MAX_QUEUED_REQUESTS} Wünsche)");
        }
        let mut tracks = req.tracks;
        let dropped = tracks.len().saturating_sub(room);
        tracks.truncate(room);
        if dropped > 0 {
            info!(
                guild_id = self.guild_id,
                dropped,
                guest = req.guest,
                "Warteschlange voll, Tracks ausgelassen"
            );
        }
        self.ensure_joined(req.voice_channel).await?;
        if req.text_channel.is_some() {
            self.text_channel = req.text_channel;
        }

        let entries: Vec<QueueEntry> = tracks
            .into_iter()
            .map(|meta| {
                let mut entry = self.new_entry(meta, req.requester);
                entry.guest = req.guest;
                entry
            })
            .collect();
        let added = entries.len();

        // Vorne einreihen, sonst hinter die anderen Wünsche, aber vor die
        // Autoplaylist – ein Wunsch soll nie hinter ihr warten.
        let index = match req
            .before
            .and_then(|b| self.queue.iter().position(|e| e.id == b))
        {
            Some(i) => i,
            None if req.play_next => 0,
            None => request_insert_index(&self.queue),
        };
        for (offset, entry) in entries.into_iter().enumerate() {
            self.queue.insert(index + offset, entry);
        }
        let position = index + 1;
        self.dirty = true;
        self.idle_since = None;

        if self.current.is_none() {
            self.advance(false).await;
            Ok(EnqueueOutcome {
                added,
                position: position.saturating_sub(1),
                dropped,
            })
        } else {
            self.prepare_next().await;
            Ok(EnqueueOutcome {
                added,
                position,
                dropped,
            })
        }
    }

    /// Automatischer Beitritt: nur, wenn der Bot nirgends ist oder in seinem
    /// Kanal niemand mehr zuhört – dann zieht er samt Wiedergabe um.
    async fn auto_join(&mut self, channel: u64) -> Result<AutoJoinOutcome> {
        if self.call.is_none() {
            return Ok(AutoJoinOutcome::Joined(self.join(channel, None).await?));
        }
        let Some(old) = self.voice_channel else {
            return Ok(AutoJoinOutcome::Joined(self.join(channel, None).await?));
        };
        if old == channel {
            return Ok(AutoJoinOutcome::AlreadyThere);
        }
        if self.listeners(old) != Some(0) {
            return Ok(AutoJoinOutcome::Busy);
        }
        let Some(current) = self.current.take() else {
            // Nichts läuft – einfach wechseln.
            return Ok(AutoJoinOutcome::Joined(self.join(channel, None).await?));
        };
        // Wiedergabe an derselben Stelle im neuen Kanal fortsetzen.
        let _ = current.handle.stop();
        self.drop_prepared();
        let keep_paused = self.paused && !self.auto_paused;
        self.paused = false;
        self.auto_paused = false;
        self.alone_since = None;
        if let Err(e) = self.ensure_joined(channel).await {
            self.queue.push_front(current.entry);
            self.dirty = true;
            return Err(e);
        }
        info!(
            guild_id = self.guild_id,
            from = old,
            to = channel,
            "Ziehe mit der Wiedergabe um"
        );
        self.start(current.entry, current.position, current.retries, false)
            .await?;
        if keep_paused {
            let _ = self.set_paused(true);
        }
        Ok(AutoJoinOutcome::Moved)
    }

    /// Betritt einen Sprachkanal (bzw. wechselt ihn, solange nichts läuft).
    /// Läuft nichts, startet die Autoplaylist; gibt es keine, greift das
    /// Inaktivitäts-Timeout.
    async fn join(&mut self, channel: u64, text_channel: Option<u64>) -> Result<Option<String>> {
        self.ensure_joined(channel).await?;
        if text_channel.is_some() {
            self.text_channel = text_channel;
        }
        if self.current.is_some() {
            return Ok(None);
        }
        self.idle_since = Some(Instant::now());
        if self.queue.is_empty() {
            Ok(self.start_autoplaylist(false).await)
        } else {
            self.advance(false).await;
            Ok(None)
        }
    }

    fn new_entry(&mut self, meta: TrackMeta, requester: u64) -> QueueEntry {
        let id = self.next_id;
        self.next_id += 1;
        QueueEntry {
            id,
            meta: Arc::new(meta),
            requester,
            autoplay: false,
            guest: false,
        }
    }

    /// Startet einen Eintrag ab `start_at`.
    async fn start(
        &mut self,
        entry: QueueEntry,
        start_at: Duration,
        retries: u8,
        announce: bool,
    ) -> Result<()> {
        let call = self
            .call
            .clone()
            .ok_or_else(|| anyhow!("Nicht mit einem Sprachkanal verbunden"))?;

        // Vorbereiteten Track übernehmen, wenn er passt.
        let prepared = match self.prepared.take() {
            Some((id, handle)) if id == entry.id && start_at.is_zero() => Some(handle),
            Some((_, handle)) => {
                let _ = handle.stop();
                None
            }
            None => None,
        };

        // Ist der vorbereitete Track inzwischen gescheitert (Handle schon
        // beendet), wird er frisch gebaut – sonst würde ein womöglich nur
        // kurz gestörter Track übersprungen.
        let prepared = prepared.and_then(|handle| {
            let _ = handle.set_volume(self.volume_factor());
            match handle.play() {
                Ok(()) => Some(handle),
                Err(e) => {
                    debug!(error = %e, "Vorbereiteter Track unbrauchbar – baue neu");
                    None
                }
            }
        });
        let handle = match prepared {
            Some(handle) => handle,
            None => {
                let track =
                    build_track(entry.meta.clone(), self.volume_factor(), start_at, retries);
                let handle = call.lock().await.play(track);
                self.watch_track(&handle)?;
                handle
            }
        };
        if self.paused {
            let _ = handle.pause();
        }

        self.current = Some(Playing {
            entry: entry.clone(),
            handle,
            start_at,
            position: start_at,
            last_progress: Instant::now(),
            retries,
            ready: false,
            loading_since: Some(Instant::now()),
        });
        if retries == 0 && start_at.is_zero() {
            metrics::TRACKS_STARTED.inc();
        }
        self.idle_since = None;
        self.dirty = true;
        if announce {
            self.emit(PlayerEvent::TrackStarted {
                guild_id: self.guild_id,
                text_channel: self.text_channel,
                entry,
            });
        }
        self.prepare_next().await;
        Ok(())
    }

    fn watch_track(&self, handle: &TrackHandle) -> Result<()> {
        let watcher = || TrackWatcher {
            tx: self.tx.clone(),
            uuid: handle.uuid(),
        };
        handle.add_event(Event::Track(TrackEvent::End), watcher())?;
        handle.add_event(Event::Track(TrackEvent::Error), watcher())?;
        Ok(())
    }

    /// Welcher Eintrag als Nächstes läuft (berücksichtigt Wiederholen).
    fn upcoming(&self) -> Option<&QueueEntry> {
        match (self.repeat, &self.current) {
            (RepeatMode::One, Some(current)) => Some(&current.entry),
            _ => self.queue.front(),
        }
    }

    /// Hält den nächsten Track pausiert und fertig geladen bereit, damit
    /// Skip und Übergänge ohne Wartezeit sind.
    async fn prepare_next(&mut self) {
        let Some(next) = self.upcoming().cloned() else {
            self.drop_prepared();
            return;
        };
        // Bei „Wiederholen: aktueller Track“ hat der vorbereitete Track
        // dieselbe Entry-ID wie der laufende, ist aber ein eigener Stream.
        if self.prepared.as_ref().is_some_and(|(id, _)| *id == next.id) {
            return;
        }
        self.drop_prepared();
        let Some(call) = self.call.clone() else {
            return;
        };
        let track = build_track(next.meta.clone(), self.volume_factor(), Duration::ZERO, 0).pause();
        let handle = call.lock().await.play(track);
        if let Err(e) = self.watch_track(&handle) {
            warn!(error = %e, "Events für vorbereiteten Track nicht registrierbar");
        }
        let _ = handle.make_playable();
        self.prepared = Some((next.id, handle));
    }

    fn drop_prepared(&mut self) {
        if let Some((_, handle)) = self.prepared.take() {
            let _ = handle.stop();
        }
    }

    /// Nächsten Track starten. `finished_normally`: der aktuelle lief zu
    /// Ende (dann greift Wiederholen).
    async fn advance(&mut self, finished_normally: bool) {
        if let Some(done) = self.current.take() {
            let _ = done.handle.stop();
            if finished_normally {
                match self.repeat {
                    RepeatMode::One => self.queue.push_front(done.entry),
                    RepeatMode::All => self.queue.push_back(done.entry),
                    RepeatMode::Off => {}
                }
            }
        }

        while let Some(next) = self.queue.pop_front() {
            match self.start(next.clone(), Duration::ZERO, 0, true).await {
                Ok(()) => return,
                Err(e) => {
                    warn!(error = %e, title = %next.meta.title, "Track konnte nicht gestartet werden");
                    metrics::TRACKS_FAILED.inc();
                    self.emit(PlayerEvent::TrackFailed {
                        guild_id: self.guild_id,
                        text_channel: self.text_channel,
                        entry: next,
                        error: format!("{e:#}"),
                    });
                    if self.call.is_none() {
                        return;
                    }
                }
            }
        }

        self.dirty = true;
        self.idle_since = Some(Instant::now());
        if self.start_autoplaylist(true).await.is_some() {
            return;
        }
        self.emit(PlayerEvent::QueueFinished {
            guild_id: self.guild_id,
            text_channel: self.text_channel,
        });
    }

    /// Startet die Autoplaylist (gemischt); liefert ihren Namen, wenn sie läuft.
    /// `require_listeners`: nur starten, wenn jemand im Kanal ist (beim
    /// automatischen Weiterspielen; bei ausdrücklichem Beitritt nicht).
    async fn start_autoplaylist(&mut self, require_listeners: bool) -> Option<String> {
        let channel = self.voice_channel?;
        if require_listeners && self.listeners(channel) == Some(0) {
            return None;
        }
        let settings = match GuildSettings::load(&self.shared.db, self.guild_id).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "Server-Einstellungen nicht lesbar");
                return None;
            }
        };
        let name = settings.autoplaylist?;
        let mut tracks = match self.shared.db.playlist_get(&name).await {
            Ok(Some(t)) if !t.is_empty() => t,
            Ok(_) => return None,
            Err(e) => {
                warn!(error = %e, "Autoplaylist nicht lesbar");
                return None;
            }
        };
        fastrand::shuffle(&mut tracks);
        info!(guild_id = self.guild_id, playlist = %name, "Starte Autoplaylist");
        for meta in tracks {
            let mut entry = self.new_entry(meta, 0);
            entry.autoplay = true;
            self.queue.push_back(entry);
        }
        let next = self.queue.pop_front()?;
        self.start(next, Duration::ZERO, 0, true)
            .await
            .ok()
            .map(|()| name)
    }

    async fn skip_current(&mut self) -> Result<()> {
        // Beim Überspringen nicht wiederholen, außer bei „ganze Warteschlange“.
        if self.repeat == RepeatMode::All
            && let Some(current) = &self.current
        {
            self.queue.push_back(current.entry.clone());
        }
        self.advance(false).await;
        Ok(())
    }

    async fn skip_to(&mut self, index: usize) -> Result<()> {
        if index >= self.queue.len() {
            bail!("Kein Track an Position {}", index + 1);
        }
        let skipped: Vec<_> = self.queue.drain(..index).collect();
        if self.repeat == RepeatMode::All {
            self.queue.extend(skipped);
        }
        self.skip_current().await
    }

    fn set_paused(&mut self, paused: bool) -> Result<()> {
        let current = self
            .current
            .as_ref()
            .ok_or_else(|| anyhow!("Es läuft gerade nichts"))?;
        if paused {
            current.handle.pause()?;
        } else {
            current.handle.play()?;
        }
        self.paused = paused;
        self.dirty = true;
        Ok(())
    }

    async fn seek(&mut self, position: Duration) -> Result<()> {
        let current = self
            .current
            .take()
            .ok_or_else(|| anyhow!("Es läuft gerade nichts"))?;
        if current.entry.meta.duration.is_none() {
            self.current = Some(current);
            bail!("Bei Livestreams kann nicht gespult werden");
        }
        if let Some(len) = current.entry.meta.duration
            && position >= len
        {
            self.current = Some(current);
            bail!("Die Position liegt hinter dem Ende des Tracks");
        }
        let _ = current.handle.stop();
        self.start(current.entry, position, current.retries, false)
            .await
    }

    fn set_volume(&mut self, volume: u16) -> Result<()> {
        if volume > lemusic_core::BotSettings::MAX_VOLUME {
            bail!(
                "Lautstärke muss zwischen 0 und {} liegen",
                lemusic_core::BotSettings::MAX_VOLUME
            );
        }
        self.volume = volume;
        if let Some(current) = &self.current {
            let _ = current.handle.set_volume(self.volume_factor());
        }
        if let Some((_, handle)) = &self.prepared {
            let _ = handle.set_volume(self.volume_factor());
        }
        self.dirty = true;
        Ok(())
    }

    fn volume_factor(&self) -> f32 {
        f32::from(self.volume) / 100.0
    }

    async fn stop(&mut self, reason: &str) -> Result<()> {
        if self.call.is_none() && self.current.is_none() && self.queue.is_empty() {
            bail!("Es läuft gerade nichts");
        }
        self.stop_with_reason(reason).await
    }

    /// Stoppt alles, leert die Warteschlange und verlässt den Kanal.
    async fn stop_with_reason(&mut self, reason: &str) -> Result<()> {
        self.queue.clear();
        self.leave_voice().await;
        self.voice_channel = None;
        self.paused = false;
        self.auto_paused = false;
        self.idle_since = None;
        self.alone_since = None;
        if let Err(e) = self.shared.db.delete(&state_key(self.guild_id)).await {
            warn!(error = %e, "Gespeicherten Zustand nicht löschbar");
        }
        self.dirty = false;
        self.emit(PlayerEvent::Left {
            guild_id: self.guild_id,
            text_channel: self.text_channel,
            reason: reason.to_string(),
        });
        self.emit(PlayerEvent::Changed {
            guild_id: self.guild_id,
        });
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Ereignisse
    // -----------------------------------------------------------------------

    async fn on_track_ended(&mut self, uuid: Uuid, error: Option<String>) {
        // Vorbereiteter Track gescheitert → einfach verwerfen, er wird beim
        // Start neu erzeugt.
        if self
            .prepared
            .as_ref()
            .is_some_and(|(_, h)| h.uuid() == uuid)
        {
            if let Some(e) = error {
                debug!(error = %e, "Vorbereiteter Track fehlgeschlagen");
            }
            self.prepared = None;
            return;
        }
        let Some(current) = &self.current else {
            return;
        };
        if current.handle.uuid() != uuid {
            return; // Veraltetes Ereignis (z. B. nach Skip).
        }

        let early_end = current
            .entry
            .meta
            .duration
            .is_some_and(|len| current.ready && current.position + EARLY_END_TOLERANCE < len);
        let failure = match (error, early_end) {
            (Some(e), _) => Some(e),
            (None, true) => Some(format!(
                "Stream endete vorzeitig bei {}",
                lemusic_core::format_duration(current.position)
            )),
            (None, false) => None,
        };

        match failure {
            None => {
                metrics::TRACKS_FINISHED.inc();
                self.advance(true).await
            }
            Some(error) => self.handle_failure(error).await,
        }
    }

    /// Fehler im laufenden Track: ab der letzten Position erneut versuchen,
    /// nach `MAX_RETRIES` überspringen.
    async fn handle_failure(&mut self, error: String) {
        let Some(current) = self.current.take() else {
            return;
        };
        let _ = current.handle.stop();
        let permanent = permanent_reason(&error);
        if permanent.is_none() && current.retries < MAX_RETRIES {
            warn!(
                title = %current.entry.meta.title,
                attempt = current.retries + 1,
                %error,
                "Track-Fehler – versuche erneut"
            );
            let resume_at = current.position;
            metrics::TRACK_RETRIES.inc();
            if self
                .start(current.entry.clone(), resume_at, current.retries + 1, false)
                .await
                .is_ok()
            {
                return;
            }
        }
        warn!(title = %current.entry.meta.title, %error, "Track übersprungen");
        metrics::TRACKS_FAILED.inc();
        self.emit(PlayerEvent::TrackFailed {
            guild_id: self.guild_id,
            text_channel: self.text_channel,
            entry: current.entry,
            error: permanent.map(str::to_string).unwrap_or(error),
        });
        self.advance(false).await;
    }

    async fn on_tick(&mut self) {
        self.update_position().await;
        self.check_stall().await;
        self.check_listeners().await;
        self.check_idle().await;
        if self.dirty || (self.current.is_some() && self.last_save.elapsed() >= SAVE_INTERVAL) {
            self.save().await;
        }
    }

    async fn update_position(&mut self) {
        let Some(current) = &mut self.current else {
            return;
        };
        let Ok(info) = current.handle.get_info().await else {
            return;
        };
        current.ready = matches!(info.ready, ReadyState::Playable);
        if current.ready
            && let Some(since) = current.loading_since.take()
        {
            metrics::TRACK_LOAD.observe(since.elapsed());
        }
        let position = current.start_at + info.position;
        if position != current.position {
            current.position = position;
            current.last_progress = Instant::now();
        }
        if let PlayMode::Errored(e) = &info.playing {
            debug!(error = %e, "Track meldet Fehler");
        }
    }

    async fn check_stall(&mut self) {
        let stalled = self
            .current
            .as_ref()
            .is_some_and(|c| c.ready && !self.paused && c.last_progress.elapsed() > STALL_TIMEOUT);
        if stalled {
            // Zustand für die Ursachenforschung festhalten.
            if let Some(current) = &self.current {
                let info = current.handle.get_info().await;
                warn!(
                    title = %current.entry.meta.title,
                    position = %lemusic_core::format_duration(current.position),
                    state = ?info.as_ref().map(|i| (&i.playing, &i.ready, i.play_time)),
                    connected = self.call.is_some(),
                    "Position bewegt sich seit 20 s nicht"
                );
            }
            metrics::STREAM_STALLS.inc();
            self.handle_failure("Stream hängt".into()).await;
        }
    }

    fn listeners(&self, channel: u64) -> Option<usize> {
        self.shared.presence.listeners(self.guild_id, channel)
    }

    /// Pausiert automatisch, wenn niemand mehr zuhört, und setzt fort, wenn
    /// wieder jemand da ist.
    async fn check_listeners(&mut self) {
        let Some(channel) = self.voice_channel else {
            return;
        };
        if self.call.is_none() {
            return;
        }
        match self.listeners(channel) {
            Some(0) => {
                if self.alone_since.is_none() {
                    self.alone_since = Some(Instant::now());
                    let pause = self.shared.settings.current().pause_when_alone;
                    if pause
                        && self.current.is_some()
                        && !self.paused
                        && self.set_paused(true).is_ok()
                    {
                        info!(guild_id = self.guild_id, "Niemand hört zu – pausiert");
                        self.auto_paused = true;
                    }
                }
            }
            Some(_) => {
                self.alone_since = None;
                if self.auto_paused {
                    self.auto_paused = false;
                    if self.set_paused(false).is_ok() {
                        info!(guild_id = self.guild_id, "Zuhörer zurück – weiter");
                    }
                }
            }
            None => {}
        }
    }

    async fn check_idle(&mut self) {
        let timeout = Duration::from_secs(self.shared.settings.current().idle_timeout_secs);
        if timeout.is_zero() || self.call.is_none() {
            return;
        }
        // Im „Bleiben“-Modus (nicht pausieren, wenn allein) zählt nur echte
        // Untätigkeit, nicht ein leerer Kanal.
        let alone_too_long = self.shared.settings.current().pause_when_alone
            && self.alone_since.is_some_and(|t| t.elapsed() >= timeout);
        let idle_too_long =
            self.current.is_none() && self.idle_since.is_some_and(|t| t.elapsed() >= timeout);
        if alone_too_long || idle_too_long {
            info!(guild_id = self.guild_id, "Inaktiv – verlasse Sprachkanal");
            let _ = self.stop_with_reason("Inaktivität").await;
        }
    }

    // -----------------------------------------------------------------------
    // Zustand
    // -----------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            guild_id: self.guild_id,
            voice_channel: self.voice_channel,
            text_channel: self.text_channel,
            now: self.current.as_ref().map(|c| NowPlaying {
                entry: c.entry.clone(),
                position: c.position,
                paused: self.paused,
                ready: c.ready,
            }),
            queue: self.queue.iter().cloned().collect(),
            repeat: self.repeat,
            volume: self.volume,
        }
    }

    fn saved_state(&self) -> Option<SavedState> {
        let voice_channel = self.voice_channel?;
        if self.current.is_none() && self.queue.is_empty() {
            return None;
        }
        Some(SavedState {
            voice_channel,
            text_channel: self.text_channel,
            current: self.current.as_ref().map(|c| SavedCurrent {
                entry: c.entry.clone(),
                position: c.position,
            }),
            queue: self.queue.iter().cloned().collect(),
            repeat: self.repeat,
            volume: self.volume,
            paused: self.paused && !self.auto_paused,
        })
    }

    async fn save(&mut self) {
        let key = state_key(self.guild_id);
        let result = match self.saved_state() {
            Some(state) => self.shared.db.set_json(&key, &state).await,
            None => self.shared.db.delete(&key).await,
        };
        match result {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
            }
            Err(e) => warn!(error = %e, "Player-Zustand nicht speicherbar"),
        }
    }

    /// Stellt einen gespeicherten Zustand wieder her (nach Neustart oder
    /// Absturz des Actors).
    /// Zustände von vor der Autoplaylist-Markierung: Einträge ohne
    /// Wünschenden, die in der eingestellten Autoplaylist vorkommen, als
    /// Autoplaylist markieren – sonst würden neue Wünsche dahinter landen.
    async fn mark_legacy_autoplay(&mut self, current: &mut Option<SavedCurrent>) {
        let unmarked = |e: &QueueEntry| e.requester == 0 && !e.autoplay;
        if !self.queue.iter().any(unmarked) && !current.as_ref().is_some_and(|c| unmarked(&c.entry))
        {
            return;
        }
        let Ok(settings) = GuildSettings::load(&self.shared.db, self.guild_id).await else {
            return;
        };
        let Some(name) = settings.autoplaylist else {
            return;
        };
        let Ok(Some(tracks)) = self.shared.db.playlist_get(&name).await else {
            return;
        };
        let urls: std::collections::HashSet<&str> = tracks.iter().map(|t| t.url.as_str()).collect();
        let mut marked = 0;
        for entry in self
            .queue
            .iter_mut()
            .chain(current.as_mut().map(|c| &mut c.entry))
        {
            if unmarked(entry) && urls.contains(entry.meta.url.as_str()) {
                entry.autoplay = true;
                marked += 1;
            }
        }
        if marked > 0 {
            info!(
                guild_id = self.guild_id,
                marked, "Ältere Autoplaylist-Einträge markiert"
            );
            self.dirty = true;
        }
    }

    async fn restore(&mut self, saved: SavedState) {
        self.next_id = saved
            .queue
            .iter()
            .chain(saved.current.as_ref().map(|c| &c.entry))
            .map(|e| e.id)
            .max()
            .unwrap_or(0)
            + 1;
        self.text_channel = saved.text_channel;
        self.repeat = saved.repeat;
        self.volume = saved.volume;
        self.paused = saved.paused;
        self.queue = saved.queue.into();
        let mut current = saved.current;
        self.mark_legacy_autoplay(&mut current).await;

        let stay = !self.shared.settings.current().pause_when_alone;
        if !stay && self.listeners(saved.voice_channel) == Some(0) {
            info!(
                guild_id = self.guild_id,
                "Niemand im Sprachkanal – Wiederherstellung übersprungen"
            );
            let _ = self.shared.db.delete(&state_key(self.guild_id)).await;
            self.queue.clear();
            return;
        }
        if let Err(e) = self.ensure_joined(saved.voice_channel).await {
            warn!(error = %e, "Wiederherstellung: Sprachkanal nicht betretbar");
            return;
        }
        info!(guild_id = self.guild_id, "Wiedergabe wiederhergestellt");
        match current {
            Some(current) => {
                if let Err(e) = self.start(current.entry, current.position, 0, false).await {
                    warn!(error = %e, "Wiederherstellung: Track nicht startbar");
                    self.advance(false).await;
                }
            }
            None => self.advance(false).await,
        }
    }

    fn emit(&self, event: PlayerEvent) {
        // Kein Empfänger ist kein Fehler.
        let _ = self.shared.events.send(event);
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
