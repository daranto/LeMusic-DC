//! Discord-Gateway-Events und Weiterleitung von Player-Ereignissen.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use lemusic_core::{Db, GuildSettings, SettingsStore};
use lemusic_player::{AutoJoinOutcome, PlayerEvent, PlayerManager};
use poise::serenity_prelude::{self as serenity, CreateMessage};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use crate::{Data, Error, embeds, presence::CachePresence};

static RESTORED: AtomicBool = AtomicBool::new(false);

pub async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &Data,
) -> Result<(), Error> {
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
        // Erst jetzt kennt der Cache die Sprachkanäle – dann kann die
        // Wiedergabe vom letzten Lauf sicher fortgesetzt werden.
        serenity::FullEvent::CacheReady { .. } if !RESTORED.swap(true, Ordering::SeqCst) => {
            data.players.restore_all().await;
            spawn_startup_auto_join(
                ctx.clone(),
                data.players.clone(),
                data.db.clone(),
                data.guild_id,
            );
        }
        serenity::FullEvent::VoiceStateUpdate { old, new } => {
            on_voice_state(ctx, data, old.as_ref(), new);
        }
        _ => {}
    }
    Ok(())
}

/// Jemand hat einen Sprachkanal betreten → ggf. automatisch beitreten.
fn on_voice_state(
    ctx: &serenity::Context,
    data: &Data,
    old: Option<&serenity::VoiceState>,
    new: &serenity::VoiceState,
) {
    if new.guild_id != Some(data.guild_id) || new.user_id == ctx.cache.current_user().id {
        return;
    }
    let Some(channel) = new.channel_id else {
        return;
    };
    if old.and_then(|o| o.channel_id) == Some(channel) {
        return; // nur Stummschalten o. Ä.
    }
    let is_bot = new
        .member
        .as_ref()
        .map(|m| m.user.bot)
        .unwrap_or_else(|| ctx.cache.user(new.user_id).map(|u| u.bot).unwrap_or(false));
    if is_bot || is_afk_channel(ctx, data.guild_id, channel) {
        return;
    }
    let players = data.players.clone();
    let db = data.db.clone();
    let guild_id = data.guild_id.get();
    tokio::spawn(async move {
        let allowed = GuildSettings::load(&db, guild_id)
            .await
            .map(|s| s.auto_join_allowed(channel.get()))
            .unwrap_or(false);
        if allowed {
            auto_join(
                &players,
                guild_id,
                channel.get(),
                "Nutzer hat den Kanal betreten",
            )
            .await;
        }
    });
}

fn is_afk_channel(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    channel: serenity::ChannelId,
) -> bool {
    ctx.cache.guild(guild_id).is_some_and(|g| {
        g.afk_metadata
            .as_ref()
            .is_some_and(|afk| afk.afk_channel_id == channel)
    })
}

async fn auto_join(players: &PlayerManager, guild_id: u64, channel: u64, reason: &str) {
    match players.player(guild_id).auto_join(channel).await {
        Ok(AutoJoinOutcome::Joined(autoplaylist)) => {
            info!(channel, reason, autoplaylist = ?autoplaylist, "Automatisch beigetreten");
        }
        Ok(AutoJoinOutcome::Moved) => {
            info!(
                channel,
                reason, "Automatisch in den Kanal mit Zuhörern umgezogen"
            );
        }
        Ok(AutoJoinOutcome::AlreadyThere | AutoJoinOutcome::Busy) => {}
        Err(e) => warn!(channel, error = %e, "Automatischer Beitritt fehlgeschlagen"),
    }
}

/// Nach dem Start: Läuft nichts (keine Wiederherstellung) und sitzt schon
/// jemand in einem erlaubten Kanal, dorthin beitreten.
fn spawn_startup_auto_join(
    ctx: serenity::Context,
    players: PlayerManager,
    db: Db,
    guild_id: serenity::GuildId,
) {
    tokio::spawn(async move {
        // Der Wiederherstellung den Vortritt lassen.
        tokio::time::sleep(Duration::from_secs(8)).await;
        let Ok(settings) = GuildSettings::load(&db, guild_id.get()).await else {
            return;
        };
        if !settings.auto_join {
            return;
        }
        let player = players.player(guild_id.get());
        if player
            .snapshot()
            .await
            .is_ok_and(|s| s.voice_channel.is_some())
        {
            return;
        }
        let channels: Vec<serenity::ChannelId> = match ctx.cache.guild(guild_id) {
            Some(g) => g.channels.keys().copied().collect(),
            None => return,
        };
        let best = channels
            .into_iter()
            .filter(|c| settings.auto_join_allowed(c.get()) && !is_afk_channel(&ctx, guild_id, *c))
            .filter_map(|c| {
                CachePresence::humans_in(&ctx.cache, guild_id, c)
                    .filter(|&n| n > 0)
                    .map(|n| (n, c))
            })
            .max_by_key(|(n, _)| *n);
        if let Some((_, channel)) = best {
            auto_join(
                &players,
                guild_id.get(),
                channel.get(),
                "nach dem Start schon jemand da",
            )
            .await;
        }
    });
}

/// Postet Player-Ereignisse („Spiele jetzt“, Fehler …) im Textkanal.
pub fn spawn_player_event_forwarder(
    ctx: serenity::Context,
    players: PlayerManager,
    db: Db,
    bot_settings: SettingsStore,
) {
    let mut rx = players.subscribe();
    tokio::spawn(async move {
        // Letzte „Spiele jetzt“-Nachricht pro Server, um sie zu ersetzen.
        let mut last_announce: HashMap<u64, serenity::Message> = HashMap::new();
        loop {
            let event = match rx.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "Player-Ereignisse verpasst");
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            let (guild_id, channel, embed, is_announce) = match event {
                PlayerEvent::TrackStarted {
                    guild_id,
                    text_channel,
                    entry,
                } => {
                    let announce = GuildSettings::load(&db, guild_id)
                        .await
                        .map(|s| s.announce_tracks)
                        .unwrap_or(true);
                    if !announce {
                        continue;
                    }
                    (guild_id, text_channel, embeds::now_playing(&entry), true)
                }
                PlayerEvent::TrackFailed {
                    guild_id,
                    text_channel,
                    entry,
                    error,
                } => {
                    let short: String = error.chars().take(200).collect();
                    (
                        guild_id,
                        text_channel,
                        embeds::warning(&format!(
                            "**{}** konnte nicht abgespielt werden und wurde übersprungen.\n`{short}`",
                            embeds::track_link(&entry)
                        )),
                        false,
                    )
                }
                PlayerEvent::QueueFinished {
                    guild_id,
                    text_channel,
                } => (
                    guild_id,
                    text_channel,
                    embeds::info("✅ Warteschlange abgespielt. Mit `/play` geht's weiter."),
                    false,
                ),
                PlayerEvent::Left {
                    guild_id,
                    text_channel,
                    reason,
                } => {
                    // „Gestoppt“ hat der Befehl selbst schon beantwortet.
                    if reason == "Gestoppt" {
                        continue;
                    }
                    info!(guild_id, %reason, "Sprachkanal verlassen");
                    (
                        guild_id,
                        text_channel,
                        embeds::info(&format!("👋 Sprachkanal verlassen: {reason}")),
                        false,
                    )
                }
                PlayerEvent::Changed { .. } => continue,
            };
            // Fester Ansage-Kanal hat Vorrang – so kommen auch Ansagen an,
            // wenn über die Weboberfläche oder die Autoplaylist gestartet wurde.
            let settings = GuildSettings::load(&db, guild_id).await.unwrap_or_default();
            let Some(channel) = settings.announce_target(channel) else {
                continue;
            };
            let channel = serenity::ChannelId::new(channel);
            match channel
                .send_message(&ctx.http, {
                    let mut message = CreateMessage::new().embed(embed);
                    // „Song hinzufügen“-Knopf zum öffentlichen Dashboard.
                    if is_announce
                        && let Some(link) = bot_settings.current().public_dashboard_link()
                    {
                        message = message.components(vec![embeds::dashboard_button(&link)]);
                    }
                    message
                })
                .await
            {
                Ok(msg) => {
                    // Alte „Spiele jetzt“-Nachricht aufräumen, damit der
                    // Kanal nicht zugemüllt wird.
                    if let Some(old) = last_announce.remove(&guild_id) {
                        let _ = old.delete(&ctx.http).await;
                    }
                    if is_announce {
                        last_announce.insert(guild_id, msg);
                    }
                }
                Err(e) => warn!(error = %e, "Nachricht konnte nicht gesendet werden"),
            }
        }
    });
}
