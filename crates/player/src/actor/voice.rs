//! Sprachkanal: Beitreten, Verlassen, Wiederverbinden, automatischer Beitritt.

use super::*;

impl Actor {
    // -----------------------------------------------------------------------
    // Sprachkanal
    // -----------------------------------------------------------------------

    pub(super) async fn ensure_joined(&mut self, channel: u64) -> Result<Arc<Mutex<Call>>> {
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

    pub(super) async fn leave_voice(&mut self) {
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

    pub(super) async fn on_disconnected(&mut self, kicked: bool) {
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
}
