//! Öffentliches Dashboard (`/public`): Wiedergabe ansehen und YouTube-/
//! SoundCloud-Links hinzufügen – ohne Admin-Login, optional mit 4-stelliger
//! PIN.
//!
//! Gäste bekommen nach der PIN-Eingabe ein eigenes Cookie. Die Sitzungen
//! liegen nur im Speicher (nach einem Neustart PIN erneut eingeben) und
//! werden ungültig, sobald der Admin die PIN ändert.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use lemusic_core::{GuildSettings, TrackMeta, auth::random_token};
use lemusic_player::{SearchSource, is_supported_link, resolve};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    WebState,
    api::{ApiError, enqueue, parse_channel},
    auth::{cookie_value, has_csrf, has_csrf_header, named_cookie_header},
    dto::PlayerDto,
};

const COOKIE: &str = "lemusic_guest";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// So viele Links dürfen Gäste insgesamt pro Minute hinzufügen.
const ADDS_PER_MINUTE: usize = 30;
/// Höchstens so viele Links auf einmal (z. B. beim Hineinziehen mehrerer).
const MAX_LINKS_PER_REQUEST: usize = 10;
/// Playlists von Gästen werden auf so viele Titel gekürzt.
const MAX_GUEST_TRACKS: usize = 200;

struct GuestSession {
    expires: Instant,
    /// PIN zum Zeitpunkt der Anmeldung – ändert der Admin sie, ist die
    /// Sitzung ungültig.
    pin: String,
}

#[derive(Default)]
pub(crate) struct PublicState {
    sessions: Mutex<HashMap<String, GuestSession>>,
    adds: Mutex<VecDeque<Instant>>,
    pub(crate) limiter: crate::auth::LoginLimiter,
}

impl PublicState {
    fn session_valid(&self, token: &str, pin: &str) -> bool {
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        sessions.retain(|_, s| s.expires > Instant::now());
        sessions.get(token).is_some_and(|s| s.pin == pin)
    }

    fn create_session(&self, pin: &str) -> Option<String> {
        let token = random_token(24);
        let mut sessions = self.sessions.lock().ok()?;
        sessions.insert(
            token.clone(),
            GuestSession {
                expires: Instant::now() + SESSION_TTL,
                pin: pin.to_string(),
            },
        );
        Some(token)
    }

    /// Zählt eine Hinzufügen-Aktion; `false`, wenn das Limit erreicht ist.
    fn allow_add(&self, count: usize) -> bool {
        let Ok(mut adds) = self.adds.lock() else {
            return false;
        };
        while adds
            .front()
            .is_some_and(|t| t.elapsed() > Duration::from_secs(60))
        {
            adds.pop_front();
        }
        if adds.len() + count > ADDS_PER_MINUTE {
            return false;
        }
        adds.extend(std::iter::repeat_n(Instant::now(), count));
        true
    }
}

fn not_enabled() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "Das öffentliche Dashboard ist deaktiviert" })),
    )
        .into_response()
}

/// Extraktor: Gast mit gültigem Zugang (Dashboard aktiv, PIN ggf. bestätigt).
pub(crate) struct Guest;

impl FromRequestParts<WebState> for Guest {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &WebState,
    ) -> Result<Self, Self::Rejection> {
        let settings = state.settings.current();
        if !settings.public_enabled {
            return Err(not_enabled());
        }
        if parts.method != axum::http::Method::GET && !has_csrf_header(parts) {
            return Err(ApiError::forbidden("Ungültige Anfrage").into_response());
        }
        if settings.public_pin.is_empty() {
            return Ok(Guest);
        }
        let valid = cookie_value(&parts.headers, COOKIE)
            .is_some_and(|t| state.public.session_valid(&t, &settings.public_pin));
        if valid {
            Ok(Guest)
        } else {
            Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "PIN erforderlich", "pin_required": true })),
            )
                .into_response())
        }
    }
}

/// Status ohne Zugangsprüfung – damit die Seite weiß, ob sie eine PIN
/// abfragen muss.
pub(crate) async fn status(State(state): State<WebState>, headers: HeaderMap) -> Response {
    let settings = state.settings.current();
    if !settings.public_enabled {
        return not_enabled();
    }
    let pin_required = !settings.public_pin.is_empty();
    let unlocked = !pin_required
        || cookie_value(&headers, COOKIE)
            .is_some_and(|t| state.public.session_valid(&t, &settings.public_pin));
    let guild = state.directory.guild_info(state.guild_id);
    Json(json!({
        "pin_required": pin_required,
        "unlocked": unlocked,
        "guild_name": guild.as_ref().map(|g| g.name.clone()),
        "bot_name": guild.as_ref().map(|g| g.bot_name.clone()),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct UnlockForm {
    pin: String,
}

pub(crate) async fn unlock(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<UnlockForm>,
) -> Result<Response, ApiError> {
    let settings = state.settings.current();
    if !settings.public_enabled {
        return Ok(not_enabled());
    }
    if !has_csrf(&headers) {
        return Err(ApiError::forbidden("Ungültige Anfrage"));
    }
    if let Some(wait) = state.public.limiter.locked_for() {
        return Err(ApiError::too_many(&format!(
            "Zu viele falsche Versuche – bitte {} s warten",
            wait.as_secs().max(1)
        )));
    }
    if settings.public_pin.is_empty() || form.pin.trim() == settings.public_pin {
        state.public.limiter.reset();
        let token = state
            .public
            .create_session(&settings.public_pin)
            .ok_or_else(|| ApiError::bad_request("Interner Fehler"))?;
        let mut response = Json(json!({ "ok": true })).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            named_cookie_header(COOKIE, &token, SESSION_TTL.as_secs()),
        );
        return Ok(response);
    }
    state.public.limiter.record_failure();
    warn!("Falsche PIN am öffentlichen Dashboard");
    tokio::time::sleep(Duration::from_millis(500)).await;
    Err(ApiError::unauthorized("Falsche PIN"))
}

/// Wiedergabe (ohne Discord-IDs der Wünschenden) und Sprachkanäle.
pub(crate) async fn player(
    State(state): State<WebState>,
    _guest: Guest,
) -> Result<Json<Value>, ApiError> {
    let snapshot = state.players.player(state.guild_id).snapshot().await?;
    let listeners = snapshot
        .voice_channel
        .and_then(|c| state.directory.listeners(state.guild_id, c));
    let mut dto = PlayerDto::new(&snapshot, listeners);
    if let Some(now) = &mut dto.now {
        now.entry.requester = None;
    }
    for entry in &mut dto.queue {
        entry.requester = None;
    }
    let settings = GuildSettings::load(&state.db, state.guild_id).await?;
    let voice_channels: Vec<_> = state
        .directory
        .guild_info(state.guild_id)
        .map(|g| g.voice_channels)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            settings
                .voice_channel
                .is_none_or(|allowed| c.id == allowed.to_string())
        })
        .collect();
    Ok(Json(
        json!({ "player": dto, "voice_channels": voice_channels }),
    ))
}

#[derive(Deserialize)]
pub(crate) struct AddForm {
    links: Vec<String>,
    voice_channel: Option<String>,
}

/// Fügt YouTube-/SoundCloud-Links hinzu (auch Playlists, gekürzt).
pub(crate) async fn add(
    State(state): State<WebState>,
    _guest: Guest,
    Json(form): Json<AddForm>,
) -> Result<Json<Value>, ApiError> {
    let mut links: Vec<String> = form
        .links
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    links.dedup();
    if links.is_empty() {
        return Err(ApiError::bad_request("Kein Link angegeben"));
    }
    if links.len() > MAX_LINKS_PER_REQUEST {
        return Err(ApiError::bad_request(format!(
            "Höchstens {MAX_LINKS_PER_REQUEST} Links auf einmal"
        )));
    }
    if let Some(bad) = links.iter().find(|l| !is_supported_link(l)) {
        return Err(ApiError::bad_request(format!(
            "Nur YouTube- und SoundCloud-Links: {}",
            bad.chars().take(80).collect::<String>()
        )));
    }
    if !state.public.allow_add(links.len()) {
        return Err(ApiError::too_many(
            "Gerade wurde viel hinzugefügt – bitte kurz warten",
        ));
    }

    // Sprachkanal: Server-Vorgabe > aktueller Kanal des Bots > Auswahl.
    let settings = GuildSettings::load(&state.db, state.guild_id).await?;
    let current = state
        .players
        .player(state.guild_id)
        .snapshot()
        .await?
        .voice_channel;
    let channel = settings
        .voice_channel
        .or(current)
        .or(parse_channel(form.voice_channel.as_deref())?)
        .ok_or_else(|| ApiError::bad_request("Bitte einen Sprachkanal auswählen"))?;

    let mut tracks: Vec<TrackMeta> = Vec::new();
    let mut failed = Vec::new();
    for link in &links {
        match resolve(link, SearchSource::YouTube).await {
            Ok(found) => tracks.extend(found),
            Err(e) => failed.push(json!({ "link": link, "error": format!("{e:#}") })),
        }
    }
    let truncated = tracks.len() > MAX_GUEST_TRACKS;
    tracks.truncate(MAX_GUEST_TRACKS);
    if tracks.is_empty() {
        return Err(ApiError::bad_request(
            failed
                .first()
                .and_then(|f| f["error"].as_str())
                .unwrap_or("Nichts gefunden")
                .to_string(),
        ));
    }

    let count = tracks.len();
    let Json(result) = enqueue(&state, tracks, channel, false).await?;
    info!(
        links = links.len(),
        tracks = count,
        "Über das öffentliche Dashboard hinzugefügt"
    );
    Ok(Json(json!({
        "added": result["added"],
        "position": result["position"],
        "skipped_too_long": result["skipped_too_long"],
        "truncated": truncated,
        "failed": failed,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_counts_links() {
        let state = PublicState::default();
        assert!(state.allow_add(ADDS_PER_MINUTE - 1));
        assert!(state.allow_add(1));
        assert!(!state.allow_add(1));
    }

    #[test]
    fn sessions_follow_pin() -> Result<(), String> {
        let state = PublicState::default();
        let token = state.create_session("1234").ok_or("kein Token")?;
        assert!(state.session_valid(&token, "1234"));
        // Admin hat die PIN geändert → alte Sitzung ungültig.
        assert!(!state.session_valid(&token, "9999"));
        assert!(!state.session_valid("unsinn", "1234"));
        Ok(())
    }
}
