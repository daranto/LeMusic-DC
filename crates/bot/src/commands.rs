//! Slash-Commands.

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow};
use lemusic_player::{SearchSource, build_track, meta_of, prepare_next, resolve};
use poise::serenity_prelude as serenity;
use songbird::{Event, TrackEvent};

use crate::{Context, Data, Error, notifier::TrackNotifier};

/// Zeit, die wir Discord für den Voice-Handshake geben.
const VOICE_TIMEOUT: Duration = Duration::from_secs(15);

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![play(), skip(), pause(), resume(), stop(), queue()]
}

fn guild_id(ctx: Context<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id()
        .ok_or_else(|| anyhow!("Nur auf einem Server nutzbar"))
}

async fn voice_manager(ctx: Context<'_>) -> Result<Arc<songbird::Songbird>> {
    songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("Songbird ist nicht initialisiert"))
}

/// Holt den Call des Servers oder tritt dem Sprachkanal des Aufrufers bei.
async fn ensure_call(ctx: Context<'_>) -> Result<Arc<tokio::sync::Mutex<songbird::Call>>> {
    let guild_id = guild_id(ctx)?;
    let manager = voice_manager(ctx).await?;

    if let Some(call) = manager.get(guild_id)
        && call.lock().await.current_channel().is_some()
    {
        return Ok(call);
    }

    let voice_channel = ctx
        .guild()
        .and_then(|g| {
            g.voice_states
                .get(&ctx.author().id)
                .and_then(|vs| vs.channel_id)
        })
        .ok_or_else(|| anyhow!("Du musst zuerst einem Sprachkanal beitreten"))?;

    let call = tokio::time::timeout(VOICE_TIMEOUT, manager.join(guild_id, voice_channel))
        .await
        .context("Zeitüberschreitung beim Betreten des Sprachkanals")??;
    call.lock().await.deafen(true).await?;
    Ok(call)
}

async fn existing_call(ctx: Context<'_>) -> Result<Arc<tokio::sync::Mutex<songbird::Call>>> {
    voice_manager(ctx)
        .await?
        .get(guild_id(ctx)?)
        .ok_or_else(|| anyhow!("Es läuft gerade nichts"))
}

/// Spielt einen YouTube-/SoundCloud-Link (auch Playlists) oder sucht.
#[poise::command(slash_command, guild_only)]
async fn play(
    ctx: Context<'_>,
    #[description = "Link oder Suchbegriff"] query: String,
    #[description = "Auf SoundCloud statt YouTube suchen"] soundcloud: Option<bool>,
) -> Result<(), Error> {
    ctx.defer().await?;
    let source = if soundcloud.unwrap_or(false) {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    };

    let tracks = resolve(&query, source).await?;
    let call = ensure_call(ctx).await?;
    let volume = ctx.data().settings.current().volume_factor();

    let reply = match tracks.as_slice() {
        [t] => format!("📥 Eingereiht: **{}** – {}", t.title, t.author),
        many => format!("📥 {} Tracks eingereiht", many.len()),
    };

    let mut handler = call.lock().await;
    for meta in tracks {
        let meta = Arc::new(meta);
        // Ohne Preload-Abfrage einreihen: sonst würde yt-dlp für jeden
        // Playlist-Eintrag sofort laufen. Vorbereitet wird gezielt nur der
        // jeweils nächste Track (siehe `prepare_next`).
        let handle = handler.enqueue_with_preload(build_track(meta.clone(), volume), None);
        let notifier = || TrackNotifier {
            text_channel: ctx.channel_id(),
            http: ctx.serenity_context().http.clone(),
            meta: meta.clone(),
            queue: handler.queue().clone(),
        };
        handle.add_event(Event::Track(TrackEvent::Play), notifier())?;
        handle.add_event(Event::Track(TrackEvent::Error), notifier())?;
    }
    // Falls gerade schon etwas läuft, ist jetzt evtl. ein neuer „nächster“
    // Track dazugekommen.
    prepare_next(handler.queue());
    drop(handler);

    ctx.say(reply).await?;
    Ok(())
}

/// Überspringt den aktuellen Track.
#[poise::command(slash_command, guild_only)]
async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    existing_call(ctx).await?.lock().await.queue().skip()?;
    ctx.say("⏭️ Übersprungen").await?;
    Ok(())
}

/// Pausiert die Wiedergabe.
#[poise::command(slash_command, guild_only)]
async fn pause(ctx: Context<'_>) -> Result<(), Error> {
    existing_call(ctx).await?.lock().await.queue().pause()?;
    ctx.say("⏸️ Pausiert").await?;
    Ok(())
}

/// Setzt die Wiedergabe fort.
#[poise::command(slash_command, guild_only)]
async fn resume(ctx: Context<'_>) -> Result<(), Error> {
    existing_call(ctx).await?.lock().await.queue().resume()?;
    ctx.say("▶️ Weiter geht's").await?;
    Ok(())
}

/// Zeigt den aktuellen und die nächsten Tracks.
#[poise::command(slash_command, guild_only)]
async fn queue(ctx: Context<'_>) -> Result<(), Error> {
    let tracks = existing_call(ctx)
        .await?
        .lock()
        .await
        .queue()
        .current_queue();
    let Some((current, upcoming)) = tracks.split_first() else {
        ctx.say("Die Warteschlange ist leer").await?;
        return Ok(());
    };

    let current = meta_of(current);
    let mut text = format!("**Läuft:** {} – {}\n", current.title, current.author);
    for (i, t) in upcoming.iter().take(10).enumerate() {
        let meta = meta_of(t);
        text.push_str(&format!("`{}.` {} – {}\n", i + 1, meta.title, meta.author));
    }
    if upcoming.len() > 10 {
        text.push_str(&format!("… und {} weitere", upcoming.len() - 10));
    }
    ctx.say(text).await?;
    Ok(())
}

/// Stoppt die Wiedergabe, leert die Warteschlange und verlässt den Kanal.
#[poise::command(slash_command, guild_only)]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let manager = voice_manager(ctx).await?;
    let guild_id = guild_id(ctx)?;
    if let Some(call) = manager.get(guild_id) {
        call.lock().await.queue().stop();
    }
    let _ = manager.remove(guild_id).await;
    ctx.say("⏹️ Gestoppt").await?;
    Ok(())
}
