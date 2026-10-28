//! Zuhörer im Sprachkanal aus dem Discord-Cache.

use std::sync::{Arc, OnceLock};

use lemusic_core::{GuildDirectory, GuildInfo, NamedId};
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

impl GuildDirectory for CachePresence {
    fn guild_info(&self, guild_id: u64) -> Option<GuildInfo> {
        let cache = self.cache.get()?;
        let bot_name = cache.current_user().name.clone();
        let guild = cache.guild(serenity::GuildId::new(guild_id))?;

        let mut channels: Vec<_> = guild.channels.values().collect();
        channels.sort_by_key(|c| (c.position, c.id));
        let named = |c: &&serenity::GuildChannel| NamedId {
            id: c.id.to_string(),
            name: c.name.clone(),
        };
        let voice_channels = channels
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    serenity::ChannelType::Voice | serenity::ChannelType::Stage
                )
            })
            .map(named)
            .collect();
        let text_channels = channels
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    serenity::ChannelType::Text | serenity::ChannelType::News
                )
            })
            .map(named)
            .collect();

        let mut roles: Vec<_> = guild
            .roles
            .values()
            .filter(|r| r.id.get() != guild_id && !r.managed)
            .collect();
        roles.sort_by_key(|r| std::cmp::Reverse(r.position));
        let roles = roles
            .into_iter()
            .map(|r| NamedId {
                id: r.id.to_string(),
                name: r.name.clone(),
            })
            .collect();

        Some(GuildInfo {
            id: guild_id.to_string(),
            name: guild.name.clone(),
            bot_name,
            voice_channels,
            text_channels,
            roles,
        })
    }

    fn listeners(&self, guild_id: u64, channel_id: u64) -> Option<usize> {
        VoicePresence::listeners(self, guild_id, channel_id)
    }
}
