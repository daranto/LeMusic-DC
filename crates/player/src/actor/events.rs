//! Ereignisse: Track-Ende, Fehler mit Wiederholung, Sekundentakt, Watchdog,
//! Zuhörer und Inaktivität.

use super::*;

impl Actor {
    // -----------------------------------------------------------------------
    // Ereignisse
    // -----------------------------------------------------------------------

    pub(super) async fn on_track_ended(&mut self, uuid: Uuid, error: Option<String>) {
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
    pub(super) async fn handle_failure(&mut self, error: String) {
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

    pub(super) async fn on_tick(&mut self) {
        self.update_position().await;
        self.check_stall().await;
        self.check_listeners().await;
        self.check_idle().await;
        if self.dirty || (self.current.is_some() && self.last_save.elapsed() >= SAVE_INTERVAL) {
            self.save().await;
        }
    }

    pub(super) async fn update_position(&mut self) {
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

    pub(super) async fn check_stall(&mut self) {
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

    pub(super) fn listeners(&self, channel: u64) -> Option<usize> {
        self.shared.presence.listeners(self.guild_id, channel)
    }

    /// Pausiert automatisch, wenn niemand mehr zuhört, und setzt fort, wenn
    /// wieder jemand da ist.
    pub(super) async fn check_listeners(&mut self) {
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

    pub(super) async fn check_idle(&mut self) {
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
}
