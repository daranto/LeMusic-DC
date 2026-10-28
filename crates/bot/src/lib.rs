//! Discord-Anbindung: Client, Slash-Commands, Events.

mod checks;
mod commands;
mod embeds;
mod events;
mod presence;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use lemusic_core::{Db, EnvConfig, Health, SettingsStore};
use lemusic_player::PlayerManager;
use poise::serenity_prelude as serenity;
use songbird::{SerenityInit, Songbird};
use tracing::{error, info, warn};

pub use presence::CachePresence;

pub(crate) type Error = anyhow::Error;
pub(crate) type Context<'a> = poise::Context<'a, Data, Error>;

pub struct Data {
    pub settings: SettingsStore,
    pub health: Arc<Health>,
    pub players: PlayerManager,
    pub db: Db,
    pub guild_id: serenity::GuildId,
    /// Vote-Skip-Stimmen: Server → (Entry-ID des Tracks, Stimmen).
    pub(crate) skip_votes: Mutex<HashMap<u64, (u64, HashSet<u64>)>>,
}

/// Alles, was der Bot von außen braucht.
pub struct BotDeps {
    pub settings: SettingsStore,
    pub health: Arc<Health>,
    pub songbird: Arc<Songbird>,
    pub players: PlayerManager,
    pub presence: Arc<CachePresence>,
    pub db: Db,
}

/// Baut den Discord-Client (startet ihn aber noch nicht).
pub async fn build_client(config: &EnvConfig, deps: BotDeps) -> Result<serenity::Client> {
    let guild_id = serenity::GuildId::new(config.guild_id);
    check_token(&config.discord_token).await?;
    let BotDeps {
        settings,
        health,
        songbird,
        players,
        presence,
        db,
    } = deps;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |err| Box::pin(on_error(err)),
            command_check: Some(|ctx| Box::pin(checks::channel_allowed(ctx))),
            // Discord erwartet binnen 3 s eine Antwort. Ist ein Player gerade
            // beschäftigt (Beitritt, Reconnect), dauert es länger – daher
            // jeden Befehl sofort quittieren.
            pre_command: |ctx| {
                Box::pin(async move {
                    if let Err(e) = ctx.defer().await {
                        warn!(error = %e, "Befehl konnte nicht quittiert werden");
                    }
                })
            },
            event_handler: |framework, event| {
                Box::pin(events::on_event(event, framework.user_data))
            },
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!(user = %ready.user.name, "Mit Discord verbunden");
                health.set_discord_connected(true);
                presence.attach(ctx.cache.clone());
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                spawn_presence_updater(ctx.clone(), settings.clone());
                events::spawn_player_event_forwarder(ctx.clone(), players.clone(), db.clone());
                Ok(Data {
                    settings,
                    health,
                    players,
                    db,
                    guild_id,
                    skip_votes: Mutex::default(),
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::GUILD_VOICE_STATES;
    let client = serenity::ClientBuilder::new(&config.discord_token, intents)
        .framework(framework)
        .register_songbird_with(songbird)
        .await?;
    Ok(client)
}

/// Prüft den Token vorab – sonst hängt der Gateway-Login bei einem falschen
/// Token still, statt mit einer klaren Meldung abzubrechen.
async fn check_token(token: &str) -> Result<()> {
    let http = serenity::Http::new(token);
    match http.get_current_user().await {
        Ok(user) => {
            info!(bot = %user.name, "Discord-Token gültig");
            Ok(())
        }
        Err(serenity::Error::Http(e))
            if e.status_code() == Some(serenity::StatusCode::UNAUTHORIZED) =>
        {
            anyhow::bail!(
                "DISCORD_TOKEN ist ungültig – im Discord Developer Portal unter „Bot“ einen neuen Token erzeugen und in .env eintragen"
            )
        }
        // Netzwerkprobleme o. Ä.: nicht abbrechen, der Client verbindet sich
        // selbst erneut.
        Err(e) => {
            warn!(error = %e, "Discord-Token nicht prüfbar – versuche trotzdem zu verbinden");
            Ok(())
        }
    }
}

/// Hält den Discord-Status aktuell, wenn sich die Einstellungen ändern.
fn spawn_presence_updater(ctx: serenity::Context, settings: SettingsStore) {
    let mut rx = settings.subscribe();
    tokio::spawn(async move {
        loop {
            let text = rx.borrow_and_update().status_text.clone();
            let activity = (!text.is_empty()).then(|| serenity::ActivityData::listening(text));
            ctx.set_activity(activity);
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
}

async fn on_error(err: poise::FrameworkError<'_, Data, Error>) {
    match err {
        poise::FrameworkError::Command { error, ctx, .. } => {
            warn!(command = %ctx.command().name, error = %error, "Befehl fehlgeschlagen");
            let reply = poise::CreateReply::default()
                .embed(embeds::error(&format!("{error:#}")))
                .ephemeral(true);
            let _ = ctx.send(reply).await;
        }
        // Die Prüfung hat dem Nutzer schon geantwortet.
        poise::FrameworkError::CommandCheckFailed { .. } => {}
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                error!(error = %e, "Fehler beim Behandeln eines Fehlers");
            }
        }
    }
}
