//! Discord-Anbindung: Client, Slash-Commands, Events.

mod commands;
mod notifier;

use std::sync::Arc;

use anyhow::Result;
use lemusic_core::{EnvConfig, Health, SettingsStore};
use poise::serenity_prelude as serenity;
use songbird::{SerenityInit, Songbird};
use tracing::{error, info, warn};

pub(crate) type Error = anyhow::Error;
pub(crate) type Context<'a> = poise::Context<'a, Data, Error>;

pub struct Data {
    pub settings: SettingsStore,
    pub health: Arc<Health>,
}

/// Alles, was der Bot von außen braucht.
pub struct BotDeps {
    pub settings: SettingsStore,
    pub health: Arc<Health>,
    pub songbird: Arc<Songbird>,
}

/// Baut den Discord-Client (startet ihn aber noch nicht).
pub async fn build_client(config: &EnvConfig, deps: BotDeps) -> Result<serenity::Client> {
    let guild_id = serenity::GuildId::new(config.guild_id);
    let BotDeps {
        settings,
        health,
        songbird,
    } = deps;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |err| Box::pin(on_error(err)),
            event_handler: |framework, event| Box::pin(on_event(event, framework.user_data)),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!(user = %ready.user.name, "Mit Discord verbunden");
                health.set_discord_connected(true);
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await?;
                spawn_presence_updater(ctx.clone(), settings.clone());
                Ok(Data { settings, health })
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

/// Verlässt alle Sprachkanäle (beim Herunterfahren).
pub async fn leave_all(songbird: &Songbird) {
    let calls: Vec<_> = songbird.iter().collect();
    for (guild_id, call) in calls {
        call.lock().await.queue().stop();
        if let Err(e) = songbird.remove(guild_id).await {
            warn!(error = %e, "Sprachkanal konnte nicht verlassen werden");
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

async fn on_event(event: &serenity::FullEvent, data: &Data) -> Result<(), Error> {
    match event {
        serenity::FullEvent::Ready { .. } | serenity::FullEvent::Resume { .. } => {
            data.health.set_discord_connected(true);
        }
        serenity::FullEvent::ShardStageUpdate { event } => {
            let connected = matches!(event.new, serenity::ConnectionStage::Connected);
            if !connected {
                warn!(stage = ?event.new, "Discord-Verbindung unterbrochen");
            }
            data.health.set_discord_connected(connected);
        }
        _ => {}
    }
    Ok(())
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
