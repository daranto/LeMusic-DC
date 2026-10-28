//! Discord-Gateway-Events und Weiterleitung von Player-Ereignissen.

use std::{collections::HashMap, sync::atomic::AtomicBool, sync::atomic::Ordering};

use lemusic_core::{Db, GuildSettings};
use lemusic_player::{PlayerEvent, PlayerManager};
use poise::serenity_prelude::{self as serenity, CreateMessage};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use crate::{Data, Error, embeds};

static RESTORED: AtomicBool = AtomicBool::new(false);

pub async fn on_event(event: &serenity::FullEvent, data: &Data) -> Result<(), Error> {
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
        }
        _ => {}
    }
    Ok(())
}

/// Postet Player-Ereignisse („Spiele jetzt“, Fehler …) im Textkanal.
pub fn spawn_player_event_forwarder(ctx: serenity::Context, players: PlayerManager, db: Db) {
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
            let Some(channel) = channel else {
                continue;
            };
            let channel = serenity::ChannelId::new(channel);
            match channel
                .send_message(&ctx.http, CreateMessage::new().embed(embed))
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
