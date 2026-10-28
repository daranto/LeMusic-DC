//! JSON-API der Weboberfläche. Alle Endpunkte erfordern Login.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use lemusic_core::{BotSettings, GuildSettings, TrackMeta, config::cookies_path, parse_duration};
use lemusic_player::{EnqueueRequest, RepeatMode, SearchSource, resolve, selftest};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use tracing::{info, warn};

use crate::{
    WebState,
    auth::Admin,
    dto::{GuildSettingsDto, PlayerDto, TrackDto},
    health_report,
};

// ---------------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------------

pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn bad_request(e: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
        }
    }
    pub fn unauthorized(msg: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }
    pub fn forbidden(msg: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }
    pub fn too_many(msg: &str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }
    fn not_found(msg: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
}

/// Fehler aus Player/DB sind fast immer Bedienfehler mit lesbarer Meldung.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::bad_request(format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn ok() -> ApiResult {
    Ok(Json(json!({ "ok": true })))
}

fn parse_channel(id: Option<&str>) -> Result<Option<u64>, ApiError> {
    id.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse()
                .map_err(|_| ApiError::bad_request("Ungültige Kanal-ID"))
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// Übersicht
// ---------------------------------------------------------------------------

static YTDLP_VERSION: Mutex<Option<(Instant, String)>> = Mutex::new(None);

async fn ytdlp_version(force: bool) -> String {
    if !force
        && let Ok(cache) = YTDLP_VERSION.lock()
        && let Some((at, v)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(300)
    {
        return v.clone();
    }
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("yt-dlp")
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let version = match output {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "nicht verfügbar".into(),
    };
    if let Ok(mut cache) = YTDLP_VERSION.lock() {
        *cache = Some((Instant::now(), version.clone()));
    }
    version
}

pub(crate) async fn overview(State(state): State<WebState>, _admin: Admin) -> ApiResult {
    let (_, health) = health_report(&state).await;
    Ok(Json(json!({
        "health": health,
        "uptime_secs": state.started.elapsed().as_secs(),
        "guild": state.directory.guild_info(state.guild_id),
        "ytdlp_version": ytdlp_version(false).await,
        "cookies": cookies_path().is_file(),
    })))
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

pub(crate) async fn player(
    State(state): State<WebState>,
    _admin: Admin,
) -> ApiResult<Json<PlayerDto>> {
    let snapshot = state.players.player(state.guild_id).snapshot().await?;
    let listeners = snapshot
        .voice_channel
        .and_then(|c| state.directory.listeners(state.guild_id, c));
    Ok(Json(PlayerDto::new(&snapshot, listeners)))
}

#[derive(Deserialize)]
pub(crate) struct PlayForm {
    query: String,
    #[serde(default)]
    soundcloud: bool,
    voice_channel: Option<String>,
    #[serde(default)]
    next: bool,
}

/// Sprachkanal: angegeben, sonst der aktuelle des Bots.
async fn voice_channel_for(state: &WebState, requested: Option<&str>) -> Result<u64, ApiError> {
    if let Some(id) = parse_channel(requested)? {
        return Ok(id);
    }
    state
        .players
        .player(state.guild_id)
        .snapshot()
        .await?
        .voice_channel
        .ok_or_else(|| ApiError::bad_request("Bitte einen Sprachkanal auswählen"))
}

async fn enqueue(
    state: &WebState,
    tracks: Vec<TrackMeta>,
    voice_channel: u64,
    next: bool,
) -> ApiResult {
    let settings = GuildSettings::load(&state.db, state.guild_id).await?;
    let (tracks, too_long): (Vec<_>, Vec<_>) = tracks.into_iter().partition(|t| {
        settings.max_track_secs == 0
            || t.duration
                .is_none_or(|d| d.as_secs() <= settings.max_track_secs)
    });
    if tracks.is_empty() {
        return Err(ApiError::bad_request("Alle Tracks sind länger als erlaubt"));
    }
    let outcome = state
        .players
        .player(state.guild_id)
        .enqueue(EnqueueRequest {
            tracks,
            requester: 0,
            voice_channel,
            text_channel: None,
            play_next: next,
        })
        .await?;
    Ok(Json(json!({
        "added": outcome.added,
        "position": outcome.position,
        "skipped_too_long": too_long.len(),
    })))
}

pub(crate) async fn play(
    State(state): State<WebState>,
    _admin: Admin,
    Json(form): Json<PlayForm>,
) -> ApiResult {
    let channel = voice_channel_for(&state, form.voice_channel.as_deref()).await?;
    let source = if form.soundcloud {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    };
    let tracks = resolve(&form.query, source).await?;
    enqueue(&state, tracks, channel, form.next).await
}

#[derive(Deserialize)]
pub(crate) struct JoinForm {
    voice_channel: Option<String>,
}

pub(crate) async fn join(
    State(state): State<WebState>,
    _admin: Admin,
    Json(form): Json<JoinForm>,
) -> ApiResult {
    let channel = parse_channel(form.voice_channel.as_deref())?
        .ok_or_else(|| ApiError::bad_request("Bitte einen Sprachkanal auswählen"))?;
    state
        .players
        .player(state.guild_id)
        .join(channel, None)
        .await?;
    ok()
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ActionForm {
    percent: Option<u16>,
    mode: Option<RepeatMode>,
    position: Option<String>,
    index: Option<usize>,
    from: Option<usize>,
    to: Option<usize>,
}

pub(crate) async fn player_action(
    State(state): State<WebState>,
    _admin: Admin,
    Path(action): Path<String>,
    form: Option<Json<ActionForm>>,
) -> ApiResult {
    let form = form.map(|Json(f)| f).unwrap_or_default();
    let player = state.players.player(state.guild_id);
    let missing = |what: &str| ApiError::bad_request(format!("{what} fehlt"));
    match action.as_str() {
        "skip" => {
            player.skip().await?;
        }
        "pause" => player.pause().await?,
        "resume" => player.resume().await?,
        "stop" => player.stop().await?,
        "shuffle" => player.shuffle().await?,
        "clear" => {
            player.clear().await?;
        }
        "volume" => {
            player
                .set_volume(form.percent.ok_or_else(|| missing("Lautstärke"))?)
                .await?
        }
        "repeat" => {
            player
                .set_repeat(form.mode.ok_or_else(|| missing("Modus"))?)
                .await?
        }
        "seek" => {
            let pos = form
                .position
                .as_deref()
                .and_then(parse_duration)
                .ok_or_else(|| ApiError::bad_request("Ungültige Zeit (z. B. 1:30)"))?;
            player.seek(pos).await?;
        }
        "remove" => {
            player
                .remove(form.index.ok_or_else(|| missing("Position"))?)
                .await?;
        }
        "move" => {
            player
                .move_entry(
                    form.from.ok_or_else(|| missing("Von"))?,
                    form.to.ok_or_else(|| missing("Nach"))?,
                )
                .await?;
        }
        "skipto" => {
            player
                .skip_to(form.index.ok_or_else(|| missing("Position"))?)
                .await?
        }
        _ => return Err(ApiError::not_found("Unbekannte Aktion")),
    }
    ok()
}

// ---------------------------------------------------------------------------
// Einstellungen
// ---------------------------------------------------------------------------

pub(crate) async fn get_bot_settings(
    State(state): State<WebState>,
    _admin: Admin,
) -> Json<BotSettings> {
    Json(state.settings.current())
}

pub(crate) async fn put_bot_settings(
    State(state): State<WebState>,
    _admin: Admin,
    Json(settings): Json<BotSettings>,
) -> ApiResult {
    state.settings.update(settings).await?;
    info!("Bot-Einstellungen geändert");
    ok()
}

pub(crate) async fn get_guild_settings(
    State(state): State<WebState>,
    _admin: Admin,
) -> ApiResult<Json<GuildSettingsDto>> {
    let settings = GuildSettings::load(&state.db, state.guild_id).await?;
    Ok(Json(GuildSettingsDto::from(&settings)))
}

pub(crate) async fn put_guild_settings(
    State(state): State<WebState>,
    _admin: Admin,
    Json(dto): Json<GuildSettingsDto>,
) -> ApiResult {
    let settings = dto.into_settings()?;
    if let Some(name) = &settings.autoplaylist
        && state.db.playlist_get(name).await?.is_none()
    {
        return Err(ApiError::bad_request(format!(
            "Playlist „{name}“ gibt es nicht"
        )));
    }
    settings.save(&state.db, state.guild_id).await?;
    info!("Server-Einstellungen geändert");
    ok()
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

pub(crate) async fn list_playlists(State(state): State<WebState>, _admin: Admin) -> ApiResult {
    Ok(Json(json!(state.db.playlist_list().await?)))
}

async fn load_playlist(state: &WebState, name: &str) -> Result<Vec<TrackMeta>, ApiError> {
    state
        .db
        .playlist_get(name)
        .await?
        .ok_or_else(|| ApiError::not_found("Playlist nicht gefunden"))
}

pub(crate) async fn get_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    let tracks = load_playlist(&state, &name).await?;
    let tracks: Vec<TrackDto> = tracks.iter().map(TrackDto::from).collect();
    Ok(Json(json!({ "name": name, "tracks": tracks })))
}

pub(crate) async fn delete_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    if !state.db.playlist_delete(&name).await? {
        return Err(ApiError::not_found("Playlist nicht gefunden"));
    }
    ok()
}

#[derive(Deserialize)]
pub(crate) struct PlayPlaylistForm {
    voice_channel: Option<String>,
    #[serde(default)]
    shuffle: bool,
}

pub(crate) async fn play_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<PlayPlaylistForm>,
) -> ApiResult {
    let mut tracks = load_playlist(&state, &name).await?;
    if tracks.is_empty() {
        return Err(ApiError::bad_request("Playlist ist leer"));
    }
    if form.shuffle {
        fastrand::shuffle(&mut tracks);
    }
    let channel = voice_channel_for(&state, form.voice_channel.as_deref()).await?;
    enqueue(&state, tracks, channel, false).await
}

#[derive(Deserialize)]
pub(crate) struct AddForm {
    query: String,
    #[serde(default)]
    soundcloud: bool,
}

pub(crate) async fn add_to_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<AddForm>,
) -> ApiResult {
    let source = if form.soundcloud {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    };
    let tracks = resolve(&form.query, source).await?;
    let total = state.db.playlist_append(&name, &tracks).await?;
    Ok(Json(json!({ "added": tracks.len(), "total": total })))
}

#[derive(Deserialize)]
pub(crate) struct IndexForm {
    index: usize,
}

pub(crate) async fn remove_from_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<IndexForm>,
) -> ApiResult {
    let mut tracks = load_playlist(&state, &name).await?;
    if form.index >= tracks.len() {
        return Err(ApiError::bad_request("Ungültige Position"));
    }
    tracks.remove(form.index);
    state.db.playlist_save(&name, &tracks).await?;
    ok()
}

#[derive(Deserialize)]
pub(crate) struct NameForm {
    name: String,
}

pub(crate) async fn save_queue_as_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Json(form): Json<NameForm>,
) -> ApiResult {
    let snapshot = state.players.player(state.guild_id).snapshot().await?;
    let tracks: Vec<TrackMeta> = snapshot
        .now
        .iter()
        .map(|n| &n.entry)
        .chain(snapshot.queue.iter())
        .map(|e| (*e.meta).clone())
        .collect();
    if tracks.is_empty() {
        return Err(ApiError::bad_request("Die Warteschlange ist leer"));
    }
    state.db.playlist_save(&form.name, &tracks).await?;
    Ok(Json(json!({ "saved": tracks.len() })))
}

// ---------------------------------------------------------------------------
// Logs & System
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct LogsQuery {
    #[serde(default)]
    after: u64,
}

pub(crate) async fn logs(
    State(state): State<WebState>,
    _admin: Admin,
    Query(q): Query<LogsQuery>,
) -> ApiResult {
    Ok(Json(json!(state.logs.since(q.after, 500))))
}

pub(crate) async fn ytdlp_update(_admin: Admin) -> ApiResult {
    info!("yt-dlp-Update über die Weboberfläche angestoßen");
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        Command::new("pip")
            .args([
                "install",
                "--no-cache-dir",
                "--quiet",
                "--upgrade",
                "yt-dlp[default,deno]",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ApiError::bad_request("Update dauerte zu lange"))?
    .map_err(|e| ApiError::bad_request(format!("pip nicht ausführbar: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(%stderr, "yt-dlp-Update fehlgeschlagen");
        return Err(ApiError::bad_request(format!(
            "Update fehlgeschlagen: {}",
            stderr.lines().last().unwrap_or("unbekannter Fehler")
        )));
    }
    let version = ytdlp_version(true).await;
    info!(%version, "yt-dlp aktualisiert");
    Ok(Json(json!({ "version": version })))
}

#[derive(Deserialize)]
pub(crate) struct SelfTestForm {
    query: String,
    #[serde(default)]
    soundcloud: bool,
}

pub(crate) async fn selftest(_admin: Admin, Json(form): Json<SelfTestForm>) -> ApiResult {
    let source = if form.soundcloud {
        SearchSource::SoundCloud
    } else {
        SearchSource::YouTube
    };
    let report = selftest::run(&form.query, source, Duration::ZERO, 10.0).await?;
    Ok(Json(json!({ "ok": report.ok(), "report": report })))
}

/// Größe, ab der eine Cookie-Datei sicher kein Cookie-Export mehr ist.
const MAX_COOKIES_BYTES: usize = 1024 * 1024;

pub(crate) async fn put_cookies(_admin: Admin, body: String) -> ApiResult {
    if body.len() > MAX_COOKIES_BYTES {
        return Err(ApiError::bad_request("Datei zu groß"));
    }
    let looks_valid = body
        .lines()
        .any(|l| l.starts_with("# Netscape HTTP Cookie File") || l.split('\t').count() >= 7);
    if !looks_valid {
        return Err(ApiError::bad_request(
            "Das ist keine Cookie-Datei im Netscape-Format (cookies.txt)",
        ));
    }
    let path = cookies_path();
    tokio::fs::write(&path, body)
        .await
        .map_err(|e| ApiError::bad_request(format!("Speichern fehlgeschlagen: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
    }
    info!("YouTube-Cookies hinterlegt");
    ok()
}

pub(crate) async fn delete_cookies(_admin: Admin) -> ApiResult {
    match tokio::fs::remove_file(cookies_path()).await {
        Ok(()) => {
            info!("YouTube-Cookies entfernt");
            ok()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ok(),
        Err(e) => Err(ApiError::bad_request(format!(
            "Löschen fehlgeschlagen: {e}"
        ))),
    }
}
