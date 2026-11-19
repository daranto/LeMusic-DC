//! Zustand: Momentaufnahme, Speichern und Wiederherstellen.

use super::*;

impl Actor {
    // -----------------------------------------------------------------------
    // Zustand
    // -----------------------------------------------------------------------

    pub(super) fn snapshot(&self) -> Snapshot {
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

    pub(super) fn saved_state(&self) -> Option<SavedState> {
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

    pub(super) async fn save(&mut self) {
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
    pub(super) async fn mark_legacy_autoplay(&mut self, current: &mut Option<SavedCurrent>) {
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

    pub(super) async fn restore(&mut self, saved: SavedState) {
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

    pub(super) fn emit(&self, event: PlayerEvent) {
        // Kein Empfänger ist kein Fehler.
        let _ = self.shared.events.send(event);
    }
}
