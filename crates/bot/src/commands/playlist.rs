//! Gespeicherte Playlists: /playlist play|save|add|list|show|delete.

use anyhow::{anyhow, bail};
use lemusic_core::TrackMeta;
use lemusic_player::{SearchSource, resolve};
use poise::{CreateReply, serenity_prelude as serenity};

use crate::{
    Context, Error,
    checks::{self, require_dj},
    commands::playback::enqueue_and_reply,
    embeds,
};

/// Gespeicherte Playlists verwalten und abspielen.
#[poise::command(
    slash_command,
    guild_only,
    subcommands("play", "save", "add", "list", "show", "delete"),
    subcommand_required
)]
pub async fn playlist(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

async fn autocomplete_name(
    ctx: Context<'_>,
    partial: &str,
) -> serenity::CreateAutocompleteResponse {
    let partial = partial.to_lowercase();
    let choices: Vec<_> = ctx
        .data()
        .db
        .playlist_list()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .filter(|n| n.to_lowercase().contains(&partial))
        .take(25)
        .map(|n| serenity::AutocompleteChoice::new(n.clone(), n))
        .collect();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

/// Spielt eine gespeicherte Playlist.
#[poise::command(slash_command, guild_only)]
async fn play(
    ctx: Context<'_>,
    #[description = "Name"]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "Zufällige Reihenfolge"] shuffle: Option<bool>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let mut tracks = ctx
        .data()
        .db
        .playlist_get(&name)
        .await?
        .ok_or_else(|| anyhow!("Playlist „{name}“ gibt es nicht"))?;
    if tracks.is_empty() {
        bail!("Playlist „{name}“ ist leer");
    }
    if shuffle.unwrap_or(false) {
        fastrand::shuffle(&mut tracks);
    }
    enqueue_and_reply(ctx, tracks, false).await
}

/// Speichert aktuellen Track + Warteschlange als Playlist (DJ).
#[poise::command(slash_command, guild_only)]
async fn save(ctx: Context<'_>, #[description = "Name"] name: String) -> Result<(), Error> {
    require_dj(ctx).await?;
    let snapshot = checks::player(ctx)?.snapshot().await?;
    let tracks: Vec<TrackMeta> = snapshot
        .now
        .iter()
        .map(|n| &n.entry)
        .chain(snapshot.queue.iter())
        .map(|e| (*e.meta).clone())
        .collect();
    if tracks.is_empty() {
        bail!("Es gibt nichts zu speichern");
    }
    ctx.data().db.playlist_save(&name, &tracks).await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "💾 Playlist **{}** mit {} Tracks gespeichert",
        name.trim(),
        tracks.len()
    ))))
    .await?;
    Ok(())
}

/// Fügt einen Link/Suchtreffer zu einer Playlist hinzu (DJ).
#[poise::command(slash_command, guild_only)]
async fn add(
    ctx: Context<'_>,
    #[description = "Name"]
    #[autocomplete = "autocomplete_name"]
    name: String,
    #[description = "Link oder Suchbegriff"] query: String,
    #[description = "Auf SoundCloud suchen"] soundcloud: Option<bool>,
) -> Result<(), Error> {
    require_dj(ctx).await?;
    ctx.defer().await?;
    let source = if soundcloud.unwrap_or(false) {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    };
    let tracks = resolve(&query, source).await?;
    let total = ctx.data().db.playlist_append(&name, &tracks).await?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "➕ {} Track(s) zu **{}** hinzugefügt (jetzt {total})",
        tracks.len(),
        name.trim()
    ))))
    .await?;
    Ok(())
}

/// Listet alle Playlists.
#[poise::command(slash_command, guild_only)]
async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let lists = ctx.data().db.playlist_list().await?;
    let text = if lists.is_empty() {
        "_Noch keine Playlists. Speichere eine mit `/playlist save`._".to_string()
    } else {
        lists
            .iter()
            .map(|p| format!("• **{}** – {} Tracks", p.name, p.track_count))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.send(CreateReply::default().embed(embeds::info(&format!("📂 **Playlists**\n\n{text}"))))
        .await?;
    Ok(())
}

/// Zeigt die Tracks einer Playlist.
#[poise::command(slash_command, guild_only)]
async fn show(
    ctx: Context<'_>,
    #[description = "Name"]
    #[autocomplete = "autocomplete_name"]
    name: String,
) -> Result<(), Error> {
    let tracks = ctx
        .data()
        .db
        .playlist_get(&name)
        .await?
        .ok_or_else(|| anyhow!("Playlist „{name}“ gibt es nicht"))?;
    let mut text: String = tracks
        .iter()
        .take(20)
        .enumerate()
        .map(|(i, t)| {
            format!(
                "`{}.` [{}]({}) `{}`\n",
                i + 1,
                t.title.replace(['[', ']'], ""),
                t.url,
                embeds::duration_label(t.duration)
            )
        })
        .collect();
    if tracks.len() > 20 {
        text.push_str(&format!("… und {} weitere", tracks.len() - 20));
    }
    let embed = serenity::CreateEmbed::new()
        .color(embeds::COLOR_MAIN)
        .title(format!("📂 {}", name.trim()))
        .description(text);
    ctx.send(CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Löscht eine Playlist (DJ).
#[poise::command(slash_command, guild_only)]
async fn delete(
    ctx: Context<'_>,
    #[description = "Name"]
    #[autocomplete = "autocomplete_name"]
    name: String,
) -> Result<(), Error> {
    require_dj(ctx).await?;
    if !ctx.data().db.playlist_delete(&name).await? {
        bail!("Playlist „{name}“ gibt es nicht");
    }
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "🗑️ Playlist **{}** gelöscht",
        name.trim()
    ))))
    .await?;
    Ok(())
}
