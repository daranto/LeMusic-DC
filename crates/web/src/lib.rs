//! Weboberfläche: statische Single-Page-App + JSON-API mit Admin-Login.

mod api;
mod assets;
mod auth;
mod dto;

use std::{future::Future, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{Context as _, Result};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
};
use lemusic_core::{Db, GuildDirectory, Health, LogBuffer, SettingsStore};
use lemusic_player::PlayerManager;
use serde::Serialize;
use tracing::info;

#[derive(Clone)]
pub struct WebState {
    pub db: Db,
    pub health: Arc<Health>,
    pub players: PlayerManager,
    pub settings: SettingsStore,
    pub directory: Arc<dyn GuildDirectory>,
    pub logs: LogBuffer,
    pub guild_id: u64,
    pub started: Instant,
    pub(crate) limiter: Arc<auth::LoginLimiter>,
}

impl WebState {
    pub fn new(
        db: Db,
        health: Arc<Health>,
        players: PlayerManager,
        settings: SettingsStore,
        directory: Arc<dyn GuildDirectory>,
        logs: LogBuffer,
        guild_id: u64,
    ) -> Self {
        Self {
            db,
            health,
            players,
            settings,
            directory,
            logs,
            guild_id,
            started: Instant::now(),
            limiter: Arc::default(),
        }
    }
}

pub fn router(state: WebState) -> Router {
    let api = Router::new()
        .route("/login", post(auth::login))
        .route("/logout", post(auth::logout))
        .route("/me", get(auth::me))
        .route("/account/password", post(auth::change_password))
        .route("/overview", get(api::overview))
        .route("/player", get(api::player))
        .route("/player/play", post(api::play))
        .route("/player/{action}", post(api::player_action))
        .route(
            "/settings/bot",
            get(api::get_bot_settings).put(api::put_bot_settings),
        )
        .route(
            "/settings/guild",
            get(api::get_guild_settings).put(api::put_guild_settings),
        )
        .route("/playlists", get(api::list_playlists))
        .route("/playlists/save-queue", post(api::save_queue_as_playlist))
        .route(
            "/playlists/{name}",
            get(api::get_playlist).delete(api::delete_playlist),
        )
        .route("/playlists/{name}/play", post(api::play_playlist))
        .route("/playlists/{name}/add", post(api::add_to_playlist))
        .route("/playlists/{name}/remove", post(api::remove_from_playlist))
        .route("/logs", get(api::logs))
        .route("/system/ytdlp-update", post(api::ytdlp_update))
        .route("/system/selftest", post(api::selftest))
        .route(
            "/system/cookies",
            put(api::put_cookies).delete(api::delete_cookies),
        );

    Router::new()
        .route("/health", get(health))
        .route("/", get(assets::index))
        .route("/app.js", get(assets::app_js))
        .route("/style.css", get(assets::style_css))
        .nest("/api", api)
        .with_state(state)
}

/// Startet den Server und läuft, bis `shutdown` fertig ist.
pub async fn serve(
    bind: SocketAddr,
    state: WebState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("Port {bind} nicht verfügbar"))?;
    info!(%bind, "Weboberfläche gestartet");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct HealthReport {
    status: &'static str,
    discord: bool,
    database: bool,
    version: &'static str,
}

pub(crate) async fn health_report(state: &WebState) -> (bool, HealthReport) {
    let database = tokio::time::timeout(std::time::Duration::from_secs(2), state.db.ping())
        .await
        .is_ok_and(|r| r.is_ok());
    let discord = state.health.discord_connected();
    let ok = database && discord;
    (
        ok,
        HealthReport {
            status: if ok { "ok" } else { "degraded" },
            discord,
            database,
            version: env!("CARGO_PKG_VERSION"),
        },
    )
}

async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let (ok, report) = health_report(&state).await;
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(report))
}
