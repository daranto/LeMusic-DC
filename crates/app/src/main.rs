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
use lemusic_bot::{BotDeps, CachePresence, build_client};
use lemusic_core::{
    Db, EnvConfig, Health, LogBuffer, LogFormat, SettingsStore, auth::AdminBootstrap,
};
use lemusic_player::PlayerManager;
use lemusic_web::WebState;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Logging auf stdout (Text oder JSON) und in den Puffer der Weboberfläche.
fn init_logging() -> LogBuffer {
    use tracing_subscriber::{
        EnvFilter, Layer as _, layer::SubscriberExt, util::SubscriberInitExt,
    };

    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,serenity=warn,songbird=warn".into())
    };
    let logs = LogBuffer::default();
    let stdout = match LogFormat::from_env() {
        LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().boxed(),
    };
    tracing_subscriber::registry()
        .with(stdout.with_filter(filter()))
        .with(logs.layer().with_filter(filter()))
        .init();
    logs
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

    let logs = init_logging();
    if let Err(e) = run(logs).await {
        error!(error = format!("{e:#}"), "Bot beendet mit Fehler");
        return Err(e);
    }
    Ok(())
}

async fn run(logs: LogBuffer) -> Result<()> {
    let config = EnvConfig::from_env()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "Starte LeMusic"
    );

    let db = Db::open(&config.database_path()).await?;
    bootstrap_admin(&db).await?;
    let settings = SettingsStore::load(db.clone()).await?;
    let health = Arc::new(Health::default());
    let songbird = songbird::Songbird::serenity();
    let presence = Arc::new(CachePresence::default());
    let players = PlayerManager::new(
        songbird.clone(),
        db.clone(),
        settings.clone(),
        presence.clone(),
    );

    // Ein Signal, auf das alle Teile beim Herunterfahren hören.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let web_state = WebState::new(
        db.clone(),
        health.clone(),
        players.clone(),
        settings.clone(),
        presence.clone(),
        logs,
        config.guild_id,
    );
    spawn_session_cleanup(db.clone());
    let mut web_shutdown = shutdown_rx.clone();
    let web = tokio::spawn(lemusic_web::serve(config.web_bind, web_state, async move {
        let _ = web_shutdown.wait_for(|stop| *stop).await;
    }));

    let mut client = build_client(
        &config,
        BotDeps {
            settings,
            health: health.clone(),
            songbird,
            players: players.clone(),
            presence,
            db: db.clone(),
        },
    )
    .await
    .context("Discord-Client konnte nicht erstellt werden")?;

    let shard_manager = client.shard_manager.clone();
    let shutdown_players = players.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        info!("Fahre herunter …");
        let _ = shutdown_tx.send(true);
        // Reihenfolge: Zustand sichern und Sprachkanäle verlassen, dann
        // Gateway schließen. Die Warteschlange läuft nach dem Neustart weiter.
        shutdown_players.shutdown().await;
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

/// Legt beim ersten Start den Admin-Account für die Weboberfläche an.
async fn bootstrap_admin(db: &Db) -> Result<()> {
    let user = std::env::var("ADMIN_USER").ok();
    let password = std::env::var("ADMIN_PASSWORD").ok();
    match db.ensure_admin(user, password).await? {
        AdminBootstrap::Existing => {}
        AdminBootstrap::CreatedFromEnv => {
            info!("Admin-Account aus ADMIN_USER/ADMIN_PASSWORD angelegt")
        }
        AdminBootstrap::CreatedRandom { username, password } => {
            // Bewusst nur auf stdout, nicht in den Log-Puffer der Weboberfläche.
            println!(
                "\n==============================================================\n\
                 Weboberfläche: Admin-Account angelegt\n  Benutzer: {username}\n  Passwort: {password}\n\
                 Bitte nach dem ersten Login unter „Konto“ ändern.\n\
                 ==============================================================\n"
            );
            warn!(
                "Kein ADMIN_PASSWORD gesetzt – zufälliges Passwort erzeugt (siehe Container-Ausgabe)"
            );
        }
    }
    Ok(())
}

/// Räumt abgelaufene Sitzungen stündlich auf.
fn spawn_session_cleanup(db: Db) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if let Err(e) = db.purge_expired_sessions().await {
                warn!(error = %e, "Sitzungen nicht aufräumbar");
            }
        }
    });
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
