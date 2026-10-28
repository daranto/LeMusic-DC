//! Wiedergabe: /play, /search, /skip, /pause, /seek, /volume …

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use lemusic_core::{GuildSettings, TrackMeta, format_duration, parse_duration};
use lemusic_player::{EnqueueRequest, SearchSource, resolve, search as search_tracks};
use poise::{
    CreateReply,
    serenity_prelude::{
        self as serenity, ComponentInteractionDataKind, CreateActionRow, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
        CreateSelectMenuOption,
    },
};

use crate::{
    Context, Error,
    checks::{self, guild_settings, in_bot_channel, is_dj, listeners, require_dj},
    embeds,
};

fn source(soundcloud: Option<bool>) -> SearchSource {
    if soundcloud.unwrap_or(false) {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    }
}

/// Filtert zu lange Tracks heraus; liefert (erlaubt, Anzahl entfernt).
fn apply_length_limit(tracks: Vec<TrackMeta>, settings: &GuildSettings) -> (Vec<TrackMeta>, usize) {
    if settings.max_track_secs == 0 {
        return (tracks, 0);
    }
    let limit = Duration::from_secs(settings.max_track_secs);
    let before = tracks.len();
    let allowed: Vec<_> = tracks
        .into_iter()
        .filter(|t| t.duration.is_none_or(|d| d <= limit))
        .collect();
    let removed = before - allowed.len();
    (allowed, removed)
}

/// Reiht Tracks ein und antwortet passend.
pub(super) async fn enqueue_and_reply(
    ctx: Context<'_>,
    tracks: Vec<TrackMeta>,
    play_next: bool,
) -> Result<()> {
    let settings = guild_settings(ctx).await?;
    let voice_channel = checks::target_voice_channel(ctx, &settings)?;
    if play_next && !is_dj(ctx, &settings).await {
        bail!("Vorne einreihen dürfen nur DJs");
    }

    let (tracks, too_long) = apply_length_limit(tracks, &settings);
    if tracks.is_empty() {
        bail!(
            "Der Track ist länger als erlaubt ({})",
            format_duration(Duration::from_secs(settings.max_track_secs))
        );
    }
    let first = tracks.first().cloned();
    let outcome = checks::player(ctx)?
        .enqueue(EnqueueRequest {
            tracks,
            requester: ctx.author().id.get(),
            voice_channel,
            text_channel: Some(ctx.channel_id().get()),
            play_next,
        })
        .await?;

    let mut text = match (outcome.added, first) {
        (1, Some(meta)) => {
            let where_ = match outcome.position {
                0 => "spielt jetzt".to_string(),
                1 => "als Nächstes".to_string(),
                n => format!("Position {n}"),
            };
            format!(
                "📥 **[{}]({})** `{}` – {where_}",
                meta.title.replace(['[', ']'], ""),
                meta.url,
                embeds::duration_label(meta.duration)
            )
        }
        (n, _) => format!("📥 **{n} Tracks** eingereiht"),
    };
    if too_long > 0 {
        text.push_str(&format!(
            "\n_{too_long} Track(s) waren zu lang und wurden ausgelassen._"
        ));
    }
    ctx.send(CreateReply::default().embed(embeds::success(&text)))
        .await?;
    Ok(())
}

/// Spielt einen YouTube-/SoundCloud-Link (auch Playlists) oder sucht.
#[poise::command(slash_command, guild_only)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "Link oder Suchbegriff"] query: String,
    #[description = "Auf SoundCloud statt YouTube suchen"] soundcloud: Option<bool>,
    #[description = "Vorne in die Warteschlange (DJ)"] next: Option<bool>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let tracks = resolve(&query, source(soundcloud)).await?;
    enqueue_and_reply(ctx, tracks, next.unwrap_or(false)).await
}

/// Sucht und lässt dich aus den Treffern wählen.
#[poise::command(slash_command, guild_only)]
pub async fn search(
    ctx: Context<'_>,
    #[description = "Suchbegriff"] query: String,
    #[description = "Auf SoundCloud statt YouTube suchen"] soundcloud: Option<bool>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let results = search_tracks(&query, source(soundcloud), 5).await?;

    let menu_id = format!("search-{}", ctx.id());
    let options: Vec<_> = results
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let label: String = t.title.chars().take(100).collect();
            let desc: String = format!("{} · {}", t.author, embeds::duration_label(t.duration))
                .chars()
                .take(100)
                .collect();
            CreateSelectMenuOption::new(label, i.to_string()).description(desc)
        })
        .collect();
    let menu = CreateSelectMenu::new(&menu_id, CreateSelectMenuKind::String { options })
        .placeholder("Track auswählen …");
    let list: String = results
        .iter()
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
    let handle = ctx
        .send(
            CreateReply::default()
                .embed(embeds::info(&format!(
                    "🔎 **Ergebnisse für „{query}“**\n\n{list}"
                )))
                .components(vec![CreateActionRow::SelectMenu(menu)]),
        )
        .await?;

    let interaction = serenity::ComponentInteractionCollector::new(ctx.serenity_context())
        .author_id(ctx.author().id)
        .channel_id(ctx.channel_id())
        .timeout(Duration::from_secs(60))
        .filter(move |i| i.data.custom_id == menu_id)
        .await;

    let Some(interaction) = interaction else {
        handle
            .edit(
                ctx,
                CreateReply::default()
                    .embed(embeds::info("⌛ Auswahl abgelaufen."))
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    };

    let chosen = match &interaction.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values
            .first()
            .and_then(|v| v.parse::<usize>().ok())
            .and_then(|i| results.get(i).cloned()),
        _ => None,
    }
    .ok_or_else(|| anyhow!("Ungültige Auswahl"))?;

    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(embeds::info(&format!(
                        "✅ Gewählt: **{}**",
                        chosen.title.replace(['[', ']'], "")
                    )))
                    .components(vec![]),
            ),
        )
        .await?;
    enqueue_and_reply(ctx, vec![chosen], false).await
}

/// Überspringt den Track – direkt für DJs und den Wünschenden, sonst per Abstimmung.
#[poise::command(slash_command, guild_only)]
pub async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = checks::guild_id(ctx)?.get();
    let player = checks::player(ctx)?;
    let snapshot = player.snapshot().await?;
    let now = snapshot
        .now
        .as_ref()
        .ok_or_else(|| anyhow!("Es läuft gerade nichts"))?;
    let settings = guild_settings(ctx).await?;
    let user = ctx.author().id.get();

    let instant = is_dj(ctx, &settings).await || now.entry.requester == user;
    if !instant {
        if !in_bot_channel(ctx, snapshot.voice_channel) {
            bail!("Du musst im selben Sprachkanal sein, um abzustimmen");
        }
        let total = snapshot
            .voice_channel
            .map(|c| listeners(ctx, c))
            .unwrap_or(1)
            .max(1);
        let required = ((total as f32) * settings.skip_ratio).ceil().max(1.0) as usize;
        let votes = {
            let mut all = ctx
                .data()
                .skip_votes
                .lock()
                .map_err(|_| anyhow!("Interner Fehler"))?;
            let entry = all.entry(guild_id).or_default();
            if entry.0 != now.entry.id {
                *entry = (now.entry.id, Default::default());
            }
            entry.1.insert(user);
            entry.1.len()
        };
        if votes < required {
            ctx.send(CreateReply::default().embed(embeds::info(&format!(
                "🗳️ Skip-Abstimmung: **{votes}/{required}** Stimmen"
            ))))
            .await?;
            return Ok(());
        }
    }

    player.skip().await?;
    if let Ok(mut all) = ctx.data().skip_votes.lock() {
        all.remove(&guild_id);
    }
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "⏭️ Übersprungen: **{}**",
        embeds::track_link(&now.entry)
    ))))
    .await?;
    Ok(())
}

/// Überspringt sofort (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn forceskip(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    let skipped = checks::player(ctx)?
        .skip()
        .await?
        .ok_or_else(|| anyhow!("Es läuft gerade nichts"))?;
    ctx.send(CreateReply::default().embed(embeds::success(&format!(
        "⏭️ Übersprungen: **{}**",
        embeds::track_link(&skipped)
    ))))
    .await?;
    Ok(())
}

/// Pausiert die Wiedergabe (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn pause(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    checks::player(ctx)?.pause().await?;
    ctx.send(CreateReply::default().embed(embeds::info("⏸️ Pausiert")))
        .await?;
    Ok(())
}

/// Setzt die Wiedergabe fort (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn resume(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    checks::player(ctx)?.resume().await?;
    ctx.send(CreateReply::default().embed(embeds::info("▶️ Weiter geht's")))
        .await?;
    Ok(())
}

/// Stoppt, leert die Warteschlange und verlässt den Kanal (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    require_dj(ctx).await?;
    checks::player(ctx)?.stop().await?;
    ctx.send(CreateReply::default().embed(embeds::info("⏹️ Gestoppt – bis zum nächsten Mal!")))
        .await?;
    Ok(())
}

/// Springt an eine Stelle im Track, z. B. 1:30.
#[poise::command(slash_command, guild_only)]
pub async fn seek(
    ctx: Context<'_>,
    #[description = "Position, z. B. 1:30 oder 90"] position: String,
) -> Result<(), Error> {
    let target = parse_duration(&position)
        .ok_or_else(|| anyhow!("Ungültige Zeit – Beispiel: 1:30 oder 1:02:03"))?;
    let player = checks::player(ctx)?;
    let snapshot = player.snapshot().await?;
    let now = snapshot
        .now
        .ok_or_else(|| anyhow!("Es läuft gerade nichts"))?;
    let settings = guild_settings(ctx).await?;
    if now.entry.requester != ctx.author().id.get() && !is_dj(ctx, &settings).await {
        bail!("Spulen dürfen nur DJs und wer den Track gewünscht hat");
    }
    ctx.defer().await?;
    player.seek(target).await?;
    ctx.send(CreateReply::default().embed(embeds::info(&format!(
        "⏩ Springe zu `{}`",
        format_duration(target)
    ))))
    .await?;
    Ok(())
}

/// Zeigt oder setzt die Lautstärke (DJ).
#[poise::command(slash_command, guild_only)]
pub async fn volume(
    ctx: Context<'_>,
    #[description = "Lautstärke in Prozent (0–200)"]
    #[min = 0]
    #[max = 200]
    percent: Option<u16>,
) -> Result<(), Error> {
    let player = checks::player(ctx)?;
    let Some(percent) = percent else {
        let current = player.snapshot().await?.volume;
        ctx.send(
            CreateReply::default().embed(embeds::info(&format!("🔊 Lautstärke: **{current} %**"))),
        )
        .await?;
        return Ok(());
    };
    require_dj(ctx).await?;
    player.set_volume(percent).await?;
    ctx.send(
        CreateReply::default().embed(embeds::info(&format!("🔊 Lautstärke: **{percent} %**"))),
    )
    .await?;
    Ok(())
}

/// Zeigt den aktuellen Track mit Fortschritt.
#[poise::command(slash_command, guild_only, rename = "nowplaying")]
pub async fn nowplaying(ctx: Context<'_>) -> Result<(), Error> {
    let snapshot = checks::player(ctx)?.snapshot().await?;
    ctx.send(CreateReply::default().embed(embeds::now_playing_detail(&snapshot)))
        .await?;
    Ok(())
}
