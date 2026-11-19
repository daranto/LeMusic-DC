//! Weboberfläche: statische Single-Page-App + JSON-API mit Admin-Login.

mod api;
mod assets;
mod auth;
mod check;
mod dto;
mod health;
mod public;

use std::{future::Future, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{Context as _, Result};
use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post, put},
};
use lemusic_core::{Db, GuildDirectory, Health, LogBuffer, SettingsStore};
use lemusic_player::PlayerManager;
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
    pub(crate) public: Arc<public::PublicState>,
    pub(crate) checks: Arc<check::Checks>,
    pub(crate) voice_watch: Arc<health::VoiceWatch>,
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
            public: Arc::default(),
            checks: Arc::default(),
            voice_watch: Arc::default(),
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
        .route("/player/join", post(api::join))
        .route("/player/queue", post(api::edit_queue))
        .route("/player/enqueue", post(api::enqueue_tracks))
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
        .route("/playlists/import", post(api::import_playlist))
        .route(
            "/playlists/{name}",
            get(api::get_playlist).delete(api::delete_playlist),
        )
        .route("/playlists/{name}/play", post(api::play_playlist))
        .route("/playlists/{name}/add", post(api::add_to_playlist))
        .route("/playlists/{name}/append", post(api::append_tracks))
        .route("/playlists/{name}/edit", post(api::edit_playlist))
        .route("/playlists/{name}/rename", post(api::rename_playlist))
        .route("/playlists/{name}/duplicate", post(api::duplicate_playlist))
        .route("/playlists/{name}/source", put(api::set_playlist_source))
        .route("/playlists/{name}/sync", post(api::sync_playlist))
        .route("/playlists/{name}/export", get(api::export_playlist))
        .route(
            "/playlists/{name}/check",
            get(api::check_status)
                .post(api::start_check)
                .delete(api::cancel_check),
        )
        .route("/logs", get(api::logs))
        .route("/system/ytdlp-update", post(api::ytdlp_update))
        .route("/system/cache", axum::routing::delete(api::clear_cache))
        .route("/system/backup", get(api::backup))
        .route("/system/selftest", post(api::selftest))
        .route(
            "/system/cookies",
            put(api::put_cookies).delete(api::delete_cookies),
        )
        .layer(axum::middleware::map_response(no_store));

    // Öffentliches Dashboard: alles unter /public/…, damit ein Reverse
    // Proxy bzw. Tunnel nur diesen Pfad freigeben muss (Admin bleibt intern).
    let public_api = Router::new()
        .route("/status", get(public::status))
        .route("/unlock", post(public::unlock))
        .route("/player", get(public::player))
        .route("/add", post(public::add))
        .layer(axum::middleware::map_response(no_store));

    Router::new()
        .route("/health", get(health::health))
        // Nicht unter /public – über einen Tunnel, der nur /public
        // durchlässt, also nicht von außen erreichbar.
        .route("/metrics", get(health::metrics))
        .route("/", get(root))
        .route("/app.js", get(assets::app_js))
        .route("/style.css", get(assets::style_css))
        .route("/dnd.js", get(assets::dnd_js))
        .route("/tracklist.js", get(assets::tracklist_js))
        .route("/public", get(assets::public_index))
        .route("/public/", get(assets::public_index))
        .route("/public/style.css", get(assets::style_css))
        .route("/public/dnd.js", get(assets::dnd_js))
        .route("/public/public.js", get(assets::public_js))
        .nest("/public/api", public_api)
        .nest("/api", api)
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state)
}

/// Sicherheits-Header für jede Antwort (auch API, Skripte, Fehler). Was eine
/// Route selbst setzt (z. B. die CSP der HTML-Seiten), bleibt unverändert.
async fn security_headers(mut response: Response) -> Response {
    use axum::http::{HeaderName, HeaderValue, header};
    let headers = response.headers_mut();
    let defaults = [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    for (name, value) in defaults {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    }
    response
}

/// API-Antworten nie zwischenspeichern (Browser, Proxys).
async fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Startseite: Über die öffentliche Adresse (z. B. Cloudflare Tunnel) direkt
/// zum öffentlichen Dashboard, intern zur Admin-Oberfläche.
async fn root(State(state): State<WebState>, headers: axum::http::HeaderMap) -> Response {
    let settings = state.settings.current();
    let request_host = request_host(&headers);
    let public_host = settings
        .public_url
        .split("://")
        .nth(1)
        .map(host_only)
        .filter(|h| !h.is_empty());
    if settings.public_enabled && request_host.is_some() && request_host == public_host {
        return Redirect::temporary("/public").into_response();
    }
    assets::index().await.into_response()
}

/// Host, an den die Anfrage ging (hinter einem Proxy aus `X-Forwarded-Host`).
pub(crate) fn request_host(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(axum::http::header::HOST))
        .and_then(|h| h.to_str().ok())
        .map(host_only)
}

/// `Example.org:8080/pfad` → `example.org`
pub(crate) fn host_only(value: &str) -> String {
    value
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
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

#[cfg(test)]
mod tests {
    use axum::{
        http::header,
        response::{Html, IntoResponse},
    };

    use super::{host_only, security_headers};

    #[tokio::test]
    async fn adds_security_headers_without_overriding() {
        let plain = security_headers(Html("x").into_response()).await;
        let h = plain.headers();
        assert_eq!(
            h.get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            h.get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("default-src 'none'; frame-ancestors 'none'")
        );

        let own = (
            [(header::CONTENT_SECURITY_POLICY, "default-src 'self'")],
            "x",
        )
            .into_response();
        let own = security_headers(own).await;
        assert_eq!(
            own.headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("default-src 'self'")
        );
    }

    #[test]
    fn host_only_strips_port_path_and_case() {
        assert_eq!(
            host_only("Le-Music.xn--gnn-sna.eu"),
            "le-music.xn--gnn-sna.eu"
        );
        assert_eq!(host_only("192.168.66.48:8080"), "192.168.66.48");
        assert_eq!(host_only("example.org/public?x=1"), "example.org");
    }
}
