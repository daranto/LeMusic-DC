//! LeMusic – Phase-0-Spike.
//!
//! Minimaler Bot, der beweist, dass die Kette Discord (inkl. DAVE) → songbird
//! → yt-dlp → YouTube/SoundCloud funktioniert. Wird in Phase 1 in Crates
//! aufgeteilt.

mod resolve;
mod selftest;
mod source;

use std::{env, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow};
use poise::serenity_prelude as serenity;
use resolve::{SearchSource, TrackMeta};
use songbird::{
    Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit, TrackEvent, tracks::Track,
};
use source::YtDlpSource;
use tracing::{error, info, warn};

type Error = anyhow::Error;
type Context<'a> = poise::Context<'a, Data, Error>;

/// Zeit, die wir Discord für den Voice-Handshake geben.
const VOICE_TIMEOUT: Duration = Duration::from_secs(15);

struct Data {}

struct Config {
    discord_token: String,
    guild_id: serenity::GuildId,
}

impl Config {
    fn from_env() -> Result<Self> {
        let var =
            |name: &str| env::var(name).with_context(|| format!("Umgebungsvariable {name} fehlt"));
        Ok(Self {
            discord_token: var("DISCORD_TOKEN")?,
            guild_id: serenity::GuildId::new(
                var("GUILD_ID")?
                    .trim()
                    .parse()
                    .context("GUILD_ID ist keine Zahl")?,
            ),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,serenity=warn,songbird=warn".into()),
        )
        .init();

    // `lemusic selftest <Link oder Suche>`: prüft yt-dlp + Dekodierung ohne Discord.
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("selftest") {
        return selftest::run(&args[1..]).await;
    }

    let config = Config::from_env()?;
    let guild_id = config.guild_id;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![play(), skip(), pause(), resume(), stop(), queue()],
            on_error: |err| Box::pin(on_error(err)),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!(user = %ready.user.name, "Mit Discord verbunden");
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                Ok(Data {})
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::GUILD_VOICE_STATES;
    let mut client = serenity::ClientBuilder::new(&config.discord_token, intents)
        .framework(framework)
        .register_songbird()
        .await?;

    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("Fahre herunter …");
        shard_manager.shutdown_all().await;
    });

    client.start().await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = ctrl_c => {},
                    _ = term.recv() => {},
                }
            }
            Err(e) => {
                warn!(error = %e, "SIGTERM-Handler konnte nicht installiert werden");
                let _ = ctrl_c.await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

async fn on_error(err: poise::FrameworkError<'_, Data, Error>) {
    match err {
        poise::FrameworkError::Command { error, ctx, .. } => {
            warn!(command = %ctx.command().name, error = %error, "Befehl fehlgeschlagen");
            let _ = ctx.say(format!("⚠️ {error}")).await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                error!(error = %e, "Fehler beim Behandeln eines Fehlers");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Track-Events
// ---------------------------------------------------------------------------

/// Meldet Start und Fehler eines Tracks im Textkanal.
struct TrackNotifier {
    text_channel: serenity::ChannelId,
    http: Arc<serenity::Http>,
    meta: Arc<TrackMeta>,
    queue: songbird::tracks::TrackQueue,
}

/// Bereitet den nächsten Track in der Warteschlange vor (yt-dlp starten,
/// herunterladen, vorpuffern), damit Skip und Übergänge ohne Wartezeit sind.
fn prepare_next(queue: &songbird::tracks::TrackQueue) {
    if let Some(next) = queue.current_queue().get(1) {
        // Ist der Track schon bereit, passiert nichts.
        let _ = next.make_playable();
    }
}

#[serenity::async_trait]
impl VoiceEventHandler for TrackNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        let EventContext::Track(states) = ctx else {
            return None;
        };
        let (state, _) = states.first()?;
        let text = match &state.playing {
            songbird::tracks::PlayMode::Play => {
                prepare_next(&self.queue);
                format!(
                    "🎶 Spiele jetzt: **{}** – {}",
                    self.meta.title, self.meta.author
                )
            }
            songbird::tracks::PlayMode::Errored(e) => {
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

// ---------------------------------------------------------------------------
// Befehle
// ---------------------------------------------------------------------------

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

    let tracks = resolve::resolve(&query, source).await?;
    let call = ensure_call(ctx).await?;

    let reply = match tracks.as_slice() {
        [t] => format!("📥 Eingereiht: **{}** – {}", t.title, t.author),
        many => format!("📥 {} Tracks eingereiht", many.len()),
    };

    let mut handler = call.lock().await;
    for meta in tracks {
        let meta = Arc::new(meta);
        let track = Track::new_with_data(YtDlpSource::input(meta.url.clone()), meta.clone());
        // Ohne Preload-Abfrage einreihen: sonst würde yt-dlp für jeden
        // Playlist-Eintrag sofort laufen. Vorbereitet wird gezielt nur der
        // jeweils nächste Track (siehe `prepare_next`).
        let handle = handler.enqueue_with_preload(track, None);
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

    let current = current.data::<TrackMeta>();
    let mut text = format!("**Läuft:** {} – {}\n", current.title, current.author);
    for (i, t) in upcoming.iter().take(10).enumerate() {
        let meta = t.data::<TrackMeta>();
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
