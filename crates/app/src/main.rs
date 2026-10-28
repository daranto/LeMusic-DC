//! LeMusic – Discord-Musikbot für YouTube und SoundCloud.
//!
//! Unterbefehle:
//! - (keiner): Bot + Weboberfläche starten
//! - `selftest [--soundcloud] <Link oder Suche>`: Audio-Kette ohne Discord prüfen
//! - `healthcheck`: fragt `/health` ab (für Docker)

mod healthcheck;
mod selftest;

use std::sync::Arc;

use anyhow::{Context as _, Result};
use lemusic_bot::{BotDeps, build_client, leave_all};
use lemusic_core::{Db, EnvConfig, Health, LogFormat, SettingsStore};
use lemusic_web::WebState;
use tokio::sync::watch;
use tracing::{error, info, warn};

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,serenity=warn,songbird=warn".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match LogFormat::from_env() {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("healthcheck") => return healthcheck::run().await,
        Some("selftest") => {
            init_logging();
            return selftest::run(&args[1..]).await;
        }
        _ => {}
    }

    init_logging();
    if let Err(e) = run().await {
        error!(error = format!("{e:#}"), "Bot beendet mit Fehler");
        return Err(e);
    }
    Ok(())
}

async fn run() -> Result<()> {
    let config = EnvConfig::from_env()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "Starte LeMusic"
    );

    let db = Db::open(&config.database_path()).await?;
    let settings = SettingsStore::load(db.clone()).await?;
    let health = Arc::new(Health::default());
    let songbird = songbird::Songbird::serenity();

    // Ein Signal, auf das alle Teile beim Herunterfahren hören.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let web_state = WebState {
        db: db.clone(),
        health: health.clone(),
    };
    let mut web_shutdown = shutdown_rx.clone();
    let web = tokio::spawn(lemusic_web::serve(config.web_bind, web_state, async move {
        let _ = web_shutdown.wait_for(|stop| *stop).await;
    }));

    let mut client = build_client(
        &config,
        BotDeps {
            settings,
            health: health.clone(),
            songbird: songbird.clone(),
        },
    )
    .await
    .context("Discord-Client konnte nicht erstellt werden")?;

    let shard_manager = client.shard_manager.clone();
    let shutdown_songbird = songbird.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        info!("Fahre herunter …");
        let _ = shutdown_tx.send(true);
        // Reihenfolge: erst Sprachkanäle sauber verlassen, dann Gateway zu.
        leave_all(&shutdown_songbird).await;
        shard_manager.shutdown_all().await;
    });

    let result = client.start().await;
    health.set_discord_connected(false);

    match web.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Webserver mit Fehler beendet"),
        Err(e) => warn!(error = %e, "Webserver-Task abgebrochen"),
    }
    db.close().await;
    info!("Beendet");
    result.context("Discord-Verbindung beendet")
}

async fn wait_for_signal() {
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
