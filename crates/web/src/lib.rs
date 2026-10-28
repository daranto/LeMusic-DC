//! HTTP-Server: Health-Endpoint (später Weboberfläche).

use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::{Context as _, Result};
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use lemusic_core::{Db, Health};
use serde::Serialize;
use tracing::info;

#[derive(Clone)]
pub struct WebState {
    pub db: Db,
    pub health: Arc<Health>,
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/health", get(health))
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
    info!(%bind, "Webserver gestartet");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[derive(Serialize)]
struct HealthReport {
    status: &'static str,
    discord: bool,
    database: bool,
    version: &'static str,
}

async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let database = tokio::time::timeout(std::time::Duration::from_secs(2), state.db.ping())
        .await
        .is_ok_and(|r| r.is_ok());
    let discord = state.health.discord_connected();
    let ok = database && discord;
    let report = HealthReport {
        status: if ok { "ok" } else { "degraded" },
        discord,
        database,
        version: env!("CARGO_PKG_VERSION"),
    };
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(report))
}
