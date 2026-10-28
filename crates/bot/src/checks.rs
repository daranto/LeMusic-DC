//! Rechte- und Kanalprüfungen.

use anyhow::{Result, anyhow, bail};
use lemusic_core::GuildSettings;
use lemusic_player::Player;
use poise::serenity_prelude as serenity;

use crate::{Context, embeds, presence::CachePresence};

pub async fn guild_settings(ctx: Context<'_>) -> Result<GuildSettings> {
    GuildSettings::load(&ctx.data().db, guild_id(ctx)?.get()).await
}

pub fn guild_id(ctx: Context<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id()
        .ok_or_else(|| anyhow!("Nur auf einem Server nutzbar"))
}

pub fn player(ctx: Context<'_>) -> Result<Player> {
    Ok(ctx.data().players.player(guild_id(ctx)?.get()))
}

/// Globale Prüfung: Befehle nur im erlaubten Textkanal.
pub async fn channel_allowed(ctx: Context<'_>) -> Result<bool, crate::Error> {
    let settings = guild_settings(ctx).await?;
    if let Some(allowed) = settings.text_channel
        && ctx.channel_id().get() != allowed
    {
        let reply = poise::CreateReply::default()
            .embed(embeds::warning(&format!(
                "Befehle bitte in {} verwenden.",
                embeds::mention_channel(allowed)
            )))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(false);
    }
    Ok(true)
}

/// Admins, Server-Manager und Mitglieder mit DJ-Rolle. Ohne DJ-Rolle sind
/// alle DJs.
pub async fn is_dj(ctx: Context<'_>, settings: &GuildSettings) -> bool {
    let Some(role) = settings.dj_role else {
        return true;
    };
    let Some(member) = ctx.author_member().await else {
        return false;
    };
    if member
        .permissions
        .is_some_and(|p| p.administrator() || p.manage_guild())
    {
        return true;
    }
    member.roles.iter().any(|r| r.get() == role)
}

pub async fn require_dj(ctx: Context<'_>) -> Result<GuildSettings> {
    let settings = guild_settings(ctx).await?;
    if !is_dj(ctx, &settings).await {
        bail!("Dafür brauchst du die DJ-Rolle");
    }
    Ok(settings)
}

/// Sprachkanal des Aufrufers.
pub fn author_voice_channel(ctx: Context<'_>) -> Option<serenity::ChannelId> {
    let guild = ctx.guild()?;
    guild
        .voice_states
        .get(&ctx.author().id)
        .and_then(|vs| vs.channel_id)
}

/// Sprachkanal, in dem gespielt werden soll – mit Prüfung der Einstellungen.
pub fn target_voice_channel(ctx: Context<'_>, settings: &GuildSettings) -> Result<u64> {
    let channel = author_voice_channel(ctx)
        .ok_or_else(|| anyhow!("Du musst zuerst einem Sprachkanal beitreten"))?;
    if let Some(allowed) = settings.voice_channel
        && channel.get() != allowed
    {
        bail!(
            "Musik gibt es nur in {} – bitte tritt dort bei",
            embeds::mention_channel(allowed)
        );
    }
    Ok(channel.get())
}

/// Ist der Aufrufer im selben Sprachkanal wie der Bot?
pub fn in_bot_channel(ctx: Context<'_>, bot_channel: Option<u64>) -> bool {
    match (author_voice_channel(ctx), bot_channel) {
        (Some(user), Some(bot)) => user.get() == bot,
        _ => false,
    }
}

/// Menschen im Sprachkanal des Bots.
pub fn listeners(ctx: Context<'_>, channel: u64) -> usize {
    let Ok(guild) = guild_id(ctx) else {
        return 0;
    };
    CachePresence::humans_in(
        &ctx.serenity_context().cache,
        guild,
        serenity::ChannelId::new(channel),
    )
    .unwrap_or(0)
}
