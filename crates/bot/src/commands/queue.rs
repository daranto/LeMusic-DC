//! Warteschlange: /queue, /remove, /move, /shuffle, /clear, /repeat, /skipto.

use anyhow::{anyhow, bail};
use lemusic_player::RepeatMode;
use poise::CreateReply;

use crate::{
    Context, Error,
    checks::{self, guild_settings, is_dj, require_dj},
    embeds,
};

/// Zeigt die Warteschlange.
#[poise::command(slash_command, guild_only)]
pub async fn queue(
    ctx: Context<'_>,
    #[description = "Seite"]
    #[min = 1]
    page: Option<usize>,
) -> Result<(), Error> {
    let snapshot = checks::player(ctx)?.snapshot().await?;
    ctx.send(CreateReply::default().embed(embeds::queue_page(&snapshot, page.unwrap_or(1))))
        .await?;
    Ok(())
}

/// Springt zu einer Position in der Warteschlange (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn skipto(
    ctx: Context<'_>,
    #[description = "Position in der Warteschlange"]
    #[min = 1]
    position: usize,
) -> Result<(), Error> {
    require_dj(ctx).await?;
    checks::player(ctx)?.skip_to(position - 1).await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "⏭️ Springe zu Position {position}"
    ))))
    .await?;
    Ok(())
}

/// Entfernt einen Track (eigene Tracks; DJs alle).
#[poise::command(slash_command, guild_only)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "Position in der Warteschlange"]
    #[min = 1]
    position: usize,
) -> Result<(), Error> {
    let player = checks::player(ctx)?;
    let snapshot = player.snapshot().await?;
    let entry = snapshot
        .queue
        .get(position - 1)
        .ok_or_else(|| anyhow!("Kein Track an Position {position}"))?;
    let settings = guild_settings(ctx).await?;
    if entry.requester != ctx.author().id.get() && !is_dj(ctx, &settings).await {
        bail!("Du kannst nur eigene Tracks entfernen");
    }
    let removed = player.remove(position - 1).await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "🗑️ Entfernt: **{}**",
        embeds::track_link(&removed)
    ))))
    .await?;
    Ok(())
}

/// Verschiebt einen Track (DJ).
#[poise::command(slash_command, guild_only, rename = "move")]
pub async fn move_track(
    ctx: Context<'_>,
    #[description = "Von Position"]
    #[min = 1]
    from: usize,
    #[description = "Nach Position"]
    #[min = 1]
    to: usize,
) -> Result<(), Error> {
    require_dj(ctx).await?;
    let moved = checks::player(ctx)?.move_entry(from - 1, to - 1).await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "↕️ **{}** → Position {to}",
        embeds::track_link(&moved)
    ))))
    .await?;
    Ok(())
}

/// Mischt die Warteschlange (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn shuffle(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    checks::player(ctx)?.shuffle().await?;
    ctx.send(CreateReply::default().embed(embeds::success("🔀 Warteschlange gemischt")))
        .await?;
    Ok(())
}

/// Leert die Warteschlange, der aktuelle Track läuft weiter (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn clear(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    let n = checks::player(ctx)?.clear().await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!("🧹 {n} Track(s) entfernt"))))
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum RepeatChoice {
    #[name = "Aus"]
    Off,
    #[name = "Aktueller Track"]
    One,
    #[name = "Ganze Warteschlange"]
    All,
}

/// Wiederholen ein-/ausschalten (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn repeat(
    ctx: Context<'_>,
    #[description = "Modus"] mode: RepeatChoice,
) -> Result<(), Error> {
    require_dj(ctx).await?;
    let mode = match mode {
        RepeatChoice::Off => RepeatMode::Off,
        RepeatChoice::One => RepeatMode::One,
        RepeatChoice::All => RepeatMode::All,
    };
    checks::player(ctx)?.set_repeat(mode).await?;
    ctx.send(CreateReply::default().embed(embeds::info(&format!(
        "🔁 Wiederholen: **{}**",
        mode.label()
    ))))
    .await?;
    Ok(())
}
