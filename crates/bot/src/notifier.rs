//! Meldet Start und Fehler eines Tracks im Textkanal.

use std::sync::Arc;

use lemusic_player::{TrackMeta, prepare_next};
use poise::serenity_prelude as serenity;
use songbird::{Event, EventContext, EventHandler, tracks::PlayMode, tracks::TrackQueue};
use tracing::warn;

pub struct TrackNotifier {
    pub text_channel: serenity::ChannelId,
    pub http: Arc<serenity::Http>,
    pub meta: Arc<TrackMeta>,
    pub queue: TrackQueue,
}

#[serenity::async_trait]
impl EventHandler for TrackNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let EventContext::Track(states) = ctx else {
            return None;
        };
        let (state, _) = states.first()?;
        let text = match &state.playing {
            PlayMode::Play => {
                prepare_next(&self.queue);
                format!(
                    "🎶 Spiele jetzt: **{}** – {}",
                    self.meta.title, self.meta.author
                )
            }
            PlayMode::Errored(e) => {
                warn!(title = %self.meta.title, error = %e, "Track fehlgeschlagen – überspringe");
                format!(
                    "⚠️ **{}** konnte nicht abgespielt werden, überspringe.",
                    self.meta.title
                )
            }
            _ => return None,
        };
        if let Err(e) = self.text_channel.say(&self.http, text).await {
            warn!(error = %e, "Nachricht konnte nicht gesendet werden");
        }
        None
    }
}
