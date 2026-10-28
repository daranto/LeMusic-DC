//! Zuhörer im Sprachkanal aus dem Discord-Cache.

use std::sync::{Arc, OnceLock};

use lemusic_player::VoicePresence;
use poise::serenity_prelude as serenity;

/// Wird vor dem Discord-Login erzeugt (der Player braucht es) und bekommt
/// den Cache, sobald die Verbindung steht.
#[derive(Default)]
pub struct CachePresence {
    cache: OnceLock<Arc<serenity::Cache>>,
}

impl CachePresence {
    pub(crate) fn attach(&self, cache: Arc<serenity::Cache>) {
        let _ = self.cache.set(cache);
    }

    /// Menschliche Nutzer im Kanal (ohne Bots).
    pub(crate) fn humans_in(
        cache: &serenity::Cache,
        guild_id: serenity::GuildId,
        channel_id: serenity::ChannelId,
    ) -> Option<usize> {
        let guild = cache.guild(guild_id)?;
        Some(
            guild
                .voice_states
                .values()
                .filter(|vs| vs.channel_id == Some(channel_id))
                .filter(|vs| {
                    let is_bot = guild
                        .members
                        .get(&vs.user_id)
                        .map(|m| m.user.bot)
                        .or_else(|| vs.member.as_ref().map(|m| m.user.bot))
                        .unwrap_or(vs.user_id == cache.current_user().id);
                    !is_bot && vs.user_id != cache.current_user().id
                })
                .count(),
        )
    }
}

impl VoicePresence for CachePresence {
    fn listeners(&self, guild_id: u64, channel_id: u64) -> Option<usize> {
        let cache = self.cache.get()?;
        Self::humans_in(
            cache,
            serenity::GuildId::new(guild_id),
            serenity::ChannelId::new(channel_id),
        )
    }
}
