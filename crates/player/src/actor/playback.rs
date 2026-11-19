//! Wiedergabe: Einreihen, Starten, Vorbereiten, Weiterschalten, Steuerung.

use super::*;

impl Actor {
    // -----------------------------------------------------------------------
    // Wiedergabe
    // -----------------------------------------------------------------------

    pub(super) async fn enqueue(&mut self, req: EnqueueRequest) -> Result<EnqueueOutcome> {
        if req.tracks.is_empty() {
            bail!("Keine Tracks");
        }
        let guest = req.guest.is_some();
        let room = enqueue_room(&self.queue, guest);
        if room == 0 {
            if guest {
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
                dropped, guest, "Warteschlange voll, Tracks ausgelassen"
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
                entry.guest = guest;
                entry.guest_id = req.guest.unwrap_or(0);
                entry
            })
            .collect();
        let added = entries.len();

        // Vorne einreihen, sonst hinter die anderen Wünsche, aber vor die
        // Autoplaylist – ein Wunsch soll nie hinter ihr warten. Mit „fair
        // mischen“ reihum nach Wünschendem.
        let before = req
            .before
            .and_then(|b| self.queue.iter().position(|e| e.id == b));
        let index = match before {
            Some(i) => Some(i),
            None if req.play_next => Some(0),
            None if self.shared.settings.current().fair_queue => None,
            None => Some(request_insert_index(&self.queue)),
        };
        let position = match index {
            Some(index) => {
                for (offset, entry) in entries.into_iter().enumerate() {
                    self.queue.insert(index + offset, entry);
                }
                index + 1
            }
            None => {
                let current = self
                    .current
                    .as_ref()
                    .filter(|c| !c.entry.autoplay)
                    .map(|c| Wisher::of(&c.entry));
                let mut first = None;
                for entry in entries {
                    let i = fair_insert_index(&self.queue, Wisher::of(&entry), current);
                    first = Some(first.map_or(i, |f: usize| f.min(i)));
                    self.queue.insert(i, entry);
                }
                first.unwrap_or(0) + 1
            }
        };
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
    pub(super) async fn auto_join(&mut self, channel: u64) -> Result<AutoJoinOutcome> {
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
    pub(super) async fn join(
        &mut self,
        channel: u64,
        text_channel: Option<u64>,
    ) -> Result<Option<String>> {
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

    pub(super) fn new_entry(&mut self, meta: TrackMeta, requester: u64) -> QueueEntry {
        let id = self.next_id;
        self.next_id += 1;
        QueueEntry {
            id,
            meta: Arc::new(meta),
            requester,
            autoplay: false,
            guest: false,
            guest_id: 0,
        }
    }

    /// Startet einen Eintrag ab `start_at`.
    pub(super) async fn start(
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

    pub(super) fn watch_track(&self, handle: &TrackHandle) -> Result<()> {
        let watcher = || TrackWatcher {
            tx: self.tx.clone(),
            uuid: handle.uuid(),
        };
        handle.add_event(Event::Track(TrackEvent::End), watcher())?;
        handle.add_event(Event::Track(TrackEvent::Error), watcher())?;
        Ok(())
    }

    /// Welcher Eintrag als Nächstes läuft (berücksichtigt Wiederholen).
    pub(super) fn upcoming(&self) -> Option<&QueueEntry> {
        match (self.repeat, &self.current) {
            (RepeatMode::One, Some(current)) => Some(&current.entry),
            _ => self.queue.front(),
        }
    }

    /// Hält den nächsten Track pausiert und fertig geladen bereit, damit
    /// Skip und Übergänge ohne Wartezeit sind.
    pub(super) async fn prepare_next(&mut self) {
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

    pub(super) fn drop_prepared(&mut self) {
        if let Some((_, handle)) = self.prepared.take() {
            let _ = handle.stop();
        }
    }

    /// Nächsten Track starten. `finished_normally`: der aktuelle lief zu
    /// Ende (dann greift Wiederholen).
    pub(super) async fn advance(&mut self, finished_normally: bool) {
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
                Ok(()) => {
                    self.remember_played(&next);
                    return;
                }
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
    pub(super) async fn start_autoplaylist(&mut self, require_listeners: bool) -> Option<String> {
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

    pub(super) async fn skip_current(&mut self) -> Result<()> {
        // Beim Überspringen nicht wiederholen, außer bei „ganze Warteschlange“.
        if self.repeat == RepeatMode::All
            && let Some(current) = &self.current
        {
            self.queue.push_back(current.entry.clone());
        }
        self.advance(false).await;
        Ok(())
    }

    pub(super) async fn skip_to(&mut self, index: usize) -> Result<()> {
        if index >= self.queue.len() {
            bail!("Kein Track an Position {}", index + 1);
        }
        let skipped: Vec<_> = self.queue.drain(..index).collect();
        if self.repeat == RepeatMode::All {
            self.queue.extend(skipped);
        }
        self.skip_current().await
    }

    pub(super) fn set_paused(&mut self, paused: bool) -> Result<()> {
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

    pub(super) async fn seek(&mut self, position: Duration) -> Result<()> {
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

    pub(super) fn set_volume(&mut self, volume: u16) -> Result<()> {
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

    pub(super) fn volume_factor(&self) -> f32 {
        f32::from(self.volume) / 100.0
    }

    pub(super) async fn stop(&mut self, reason: &str) -> Result<()> {
        if self.call.is_none() && self.current.is_none() && self.queue.is_empty() {
            bail!("Es läuft gerade nichts");
        }
        self.stop_with_reason(reason).await
    }

    /// Stoppt alles, leert die Warteschlange und verlässt den Kanal.
    pub(super) async fn stop_with_reason(&mut self, reason: &str) -> Result<()> {
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
}
