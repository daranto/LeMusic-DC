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
use lemusic_core::{
    BotSettings, GuildSettings, Playlist, PlaylistConflict, PlaylistOp, SortKey, TrackMeta,
    config::cookies_path,
    parse_duration,
    playlists::{apply_ops, validate_name},
};
use lemusic_player::{
    EnqueueRequest, QueueEdit, RepeatMode, SearchSource, is_supported_link, is_url, resolve,
    selftest, ytdlp,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::{
    WebState,
    auth::Admin,
    dto::{GuildSettingsDto, PlayerDto, TrackDto, TrackInput},
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
        if e.downcast_ref::<PlaylistConflict>().is_some() {
            return Self {
                status: StatusCode::CONFLICT,
                message: e.to_string(),
            };
        }
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

pub(crate) fn parse_channel(id: Option<&str>) -> Result<Option<u64>, ApiError> {
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
    let version = ytdlp::version()
        .await
        .unwrap_or_else(|| "nicht verfügbar".into());
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
        "ytdlp_last_update": ytdlp::last_status(&state.db).await,
        "time_zone": ytdlp::time_zone_name(),
        "cache": tokio::task::spawn_blocking(lemusic_player::cache::stats).await.ok(),
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

pub(crate) async fn enqueue(
    state: &WebState,
    tracks: Vec<TrackMeta>,
    voice_channel: u64,
    next: bool,
    before: Option<u64>,
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
            before,
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
    enqueue(&state, tracks, channel, form.next, None).await
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
    let autoplaylist = state
        .players
        .player(state.guild_id)
        .join(channel, None)
        .await?;
    Ok(Json(json!({ "ok": true, "autoplaylist": autoplaylist })))
}

/// Warteschlange bearbeiten (Mehrfachauswahl, Drag & Drop, Rückgängig).
pub(crate) async fn edit_queue(
    State(state): State<WebState>,
    _admin: Admin,
    Json(edit): Json<QueueEdit>,
) -> ApiResult {
    let affected = state
        .players
        .player(state.guild_id)
        .edit_queue(edit)
        .await?;
    Ok(Json(json!({ "affected": affected })))
}

#[derive(Deserialize)]
pub(crate) struct EnqueueTracksForm {
    tracks: Vec<TrackInput>,
    voice_channel: Option<String>,
    /// Als Nächstes einreihen.
    #[serde(default)]
    next: bool,
    /// Sofort spielen (aktueller Track wird übersprungen).
    #[serde(default)]
    now: bool,
    /// Vor diesem Warteschlangen-Eintrag einreihen.
    #[serde(default)]
    before: Option<u64>,
}

/// Bereits bekannte Titel einreihen (aus einer Playlist gezogen) – ohne
/// erneutes Auflösen über yt-dlp.
pub(crate) async fn enqueue_tracks(
    State(state): State<WebState>,
    _admin: Admin,
    Json(form): Json<EnqueueTracksForm>,
) -> ApiResult {
    let tracks = TrackInput::into_metas(form.tracks)?;
    if tracks.is_empty() || tracks.len() > lemusic_core::playlists::MAX_TRACKS {
        return Err(ApiError::bad_request("Ungültige Anzahl Titel"));
    }
    let channel = voice_channel_for(&state, form.voice_channel.as_deref()).await?;
    let result = enqueue(&state, tracks, channel, form.next || form.now, form.before).await?;
    if form.now && result.0.get("position").and_then(Value::as_u64) == Some(1) {
        state.players.player(state.guild_id).skip().await?;
    }
    Ok(result)
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

async fn load_playlist(state: &WebState, name: &str) -> Result<Playlist, ApiError> {
    state
        .db
        .playlist_load(name)
        .await?
        .ok_or_else(|| ApiError::not_found("Playlist nicht gefunden"))
}

fn playlist_json(p: &Playlist) -> Value {
    let tracks: Vec<TrackDto> = p.tracks.iter().map(TrackDto::from).collect();
    let duration: u64 = p
        .tracks
        .iter()
        .filter_map(|t| t.duration)
        .map(|d| d.as_secs())
        .sum();
    json!({
        "name": p.name,
        "revision": p.revision,
        "source": p.source,
        "updated_at": p.updated_at,
        "duration": duration,
        "tracks": tracks,
    })
}

async fn playlist_response(state: &WebState, name: &str, extra: Value) -> ApiResult {
    let p = load_playlist(state, name).await?;
    let mut out = playlist_json(&p);
    if let (Some(out), Value::Object(extra)) = (out.as_object_mut(), extra) {
        out.extend(extra);
    }
    Ok(Json(out))
}

pub(crate) async fn get_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    playlist_response(&state, &name, json!({})).await
}

pub(crate) async fn delete_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    if !state.db.playlist_delete(&name).await? {
        return Err(ApiError::not_found("Playlist nicht gefunden"));
    }
    state.checks.forget(&name);
    ok()
}

#[derive(Deserialize)]
pub(crate) struct PlayPlaylistForm {
    voice_channel: Option<String>,
    #[serde(default)]
    shuffle: bool,
    /// Ab dieser Position (0-basiert) abspielen.
    #[serde(default)]
    start: usize,
}

pub(crate) async fn play_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<PlayPlaylistForm>,
) -> ApiResult {
    let mut tracks = load_playlist(&state, &name).await?.tracks;
    if form.start > 0 {
        tracks = tracks.into_iter().skip(form.start).collect();
    }
    if tracks.is_empty() {
        return Err(ApiError::bad_request("Playlist ist leer"));
    }
    if form.shuffle {
        fastrand::shuffle(&mut tracks);
    }
    let channel = voice_channel_for(&state, form.voice_channel.as_deref()).await?;
    enqueue(&state, tracks, channel, false, None).await
}

#[derive(Deserialize)]
pub(crate) struct AddForm {
    query: String,
    #[serde(default)]
    soundcloud: bool,
}

/// Link oder Suche auflösen und anhängen. Wird eine neue (oder leere)
/// Playlist aus einem Playlist-Link gefüllt, merkt sie sich den Link als
/// Quelle für „Aktualisieren“.
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
    let was_empty = state
        .db
        .playlist_load(&name)
        .await?
        .is_none_or(|p| p.tracks.is_empty() && p.source.is_none());
    let total = state.db.playlist_append(&name, &tracks).await?;
    let query = form.query.trim();
    if was_empty && tracks.len() > 1 && is_url(query) {
        state.db.playlist_set_source(&name, Some(query)).await?;
    }
    Ok(Json(json!({ "added": tracks.len(), "total": total })))
}

#[derive(Deserialize)]
pub(crate) struct TracksForm {
    tracks: Vec<TrackInput>,
}

/// Titel direkt anhängen (aus der Warteschlange oder einer anderen Playlist).
pub(crate) async fn append_tracks(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<TracksForm>,
) -> ApiResult {
    let tracks = TrackInput::into_metas(form.tracks)?;
    if tracks.is_empty() {
        return Err(ApiError::bad_request("Keine Titel"));
    }
    let total = state.db.playlist_append(&name, &tracks).await?;
    Ok(Json(json!({ "added": tracks.len(), "total": total })))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum OpDto {
    Remove {
        indices: Vec<usize>,
    },
    Move {
        indices: Vec<usize>,
        to: usize,
    },
    Insert {
        index: usize,
        tracks: Vec<TrackInput>,
    },
    Sort {
        by: SortKey,
    },
    Dedupe,
    Replace {
        tracks: Vec<TrackInput>,
    },
}

impl OpDto {
    fn into_op(self) -> anyhow::Result<PlaylistOp> {
        Ok(match self {
            Self::Remove { indices } => PlaylistOp::Remove(indices),
            Self::Move { indices, to } => PlaylistOp::Move { indices, to },
            Self::Insert { index, tracks } => PlaylistOp::Insert {
                index,
                tracks: TrackInput::into_metas(tracks)?,
            },
            Self::Sort { by } => PlaylistOp::Sort(by),
            Self::Dedupe => PlaylistOp::Dedupe,
            Self::Replace { tracks } => PlaylistOp::Replace(TrackInput::into_metas(tracks)?),
        })
    }
}

#[derive(Deserialize)]
pub(crate) struct EditForm {
    revision: i64,
    ops: Vec<OpDto>,
}

/// Bearbeitet eine Playlist. `revision` muss dem aktuellen Stand
/// entsprechen, sonst 409 (die Oberfläche lädt dann neu).
pub(crate) async fn edit_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<EditForm>,
) -> ApiResult {
    if form.ops.is_empty() || form.ops.len() > 100 {
        return Err(ApiError::bad_request("Ungültige Bearbeitung"));
    }
    let ops = form
        .ops
        .into_iter()
        .map(OpDto::into_op)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let playlist = load_playlist(&state, &name).await?;
    if playlist.revision != form.revision {
        return Err(ApiError::from(anyhow::Error::new(PlaylistConflict)));
    }
    let mut tracks = playlist.tracks;
    let affected = apply_ops(&mut tracks, ops)?;
    state
        .db
        .playlist_update(&name, form.revision, &tracks)
        .await?;
    playlist_response(&state, &name, json!({ "affected": affected })).await
}

#[derive(Deserialize)]
pub(crate) struct NameForm {
    name: String,
}

/// Umbenennen – eine eingestellte Autoplaylist zieht mit.
pub(crate) async fn rename_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<NameForm>,
) -> ApiResult {
    let new = validate_name(&form.name)?.to_string();
    state.db.playlist_rename(&name, &new).await?;
    state.checks.forget(&name);
    let mut settings = GuildSettings::load(&state.db, state.guild_id).await?;
    if settings
        .autoplaylist
        .as_deref()
        .is_some_and(|a| a.eq_ignore_ascii_case(name.trim()))
    {
        settings.autoplaylist = Some(new.clone());
        settings.save(&state.db, state.guild_id).await?;
    }
    info!(old = %name, new = %new, "Playlist umbenannt");
    Ok(Json(json!({ "name": new })))
}

pub(crate) async fn duplicate_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<NameForm>,
) -> ApiResult {
    let p = load_playlist(&state, &name).await?;
    state
        .db
        .playlist_create(&form.name, &p.tracks, p.source.as_deref())
        .await?;
    Ok(Json(json!({ "name": form.name.trim() })))
}

#[derive(Deserialize)]
pub(crate) struct SourceForm {
    source: Option<String>,
}

pub(crate) async fn set_playlist_source(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
    Json(form): Json<SourceForm>,
) -> ApiResult {
    load_playlist(&state, &name).await?;
    let source = form
        .source
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(s) = &source
        && !is_supported_link(s)
    {
        return Err(ApiError::bad_request(
            "Bitte einen YouTube- oder SoundCloud-Playlist-Link angeben",
        ));
    }
    state
        .db
        .playlist_set_source(&name, source.as_deref())
        .await?;
    ok()
}

/// Holt neue Titel aus der Quell-Playlist (bereits enthaltene bleiben).
pub(crate) async fn sync_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    let p = load_playlist(&state, &name).await?;
    let source = p
        .source
        .ok_or_else(|| ApiError::bad_request("Für diese Playlist ist keine Quelle hinterlegt"))?;
    let fetched = resolve(&source, SearchSource::YouTube).await?;
    let known: std::collections::HashSet<String> = p.tracks.iter().map(TrackMeta::key).collect();
    let mut seen = known.clone();
    let new: Vec<TrackMeta> = fetched
        .into_iter()
        .filter(|t| seen.insert(t.key()))
        .collect();
    let added = new.len();
    if added > 0 {
        let mut tracks = p.tracks;
        tracks.extend(new);
        state.db.playlist_update(&name, p.revision, &tracks).await?;
    }
    info!(playlist = %name, added, "Playlist aus Quelle aktualisiert");
    playlist_response(&state, &name, json!({ "added": added })).await
}

pub(crate) async fn export_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    let p = load_playlist(&state, &name).await?;
    let tracks: Vec<TrackDto> = p.tracks.iter().map(TrackDto::from).collect();
    Ok(Json(json!({
        "lemusic_playlist": 1,
        "name": p.name,
        "source": p.source,
        "tracks": tracks,
    })))
}

#[derive(Deserialize)]
pub(crate) struct ImportForm {
    name: String,
    #[serde(default)]
    source: Option<String>,
    tracks: Vec<TrackInput>,
}

pub(crate) async fn import_playlist(
    State(state): State<WebState>,
    _admin: Admin,
    Json(form): Json<ImportForm>,
) -> ApiResult {
    let tracks = TrackInput::into_metas(form.tracks)?;
    let source = form.source.filter(|s| is_supported_link(s));
    state
        .db
        .playlist_create(&form.name, &tracks, source.as_deref())
        .await?;
    info!(playlist = %form.name.trim(), tracks = tracks.len(), "Playlist importiert");
    Ok(Json(
        json!({ "name": form.name.trim(), "total": tracks.len() }),
    ))
}

pub(crate) async fn start_check(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    let p = load_playlist(&state, &name).await?;
    if p.tracks.is_empty() {
        return Err(ApiError::bad_request("Playlist ist leer"));
    }
    state
        .checks
        .start(&p.name, p.tracks)
        .map_err(ApiError::bad_request)?;
    ok()
}

pub(crate) async fn check_status(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    Ok(Json(json!(state.checks.status(&name))))
}

pub(crate) async fn cancel_check(
    State(state): State<WebState>,
    _admin: Admin,
    Path(name): Path<String>,
) -> ApiResult {
    state.checks.cancel(&name);
    ok()
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

pub(crate) async fn clear_cache(_admin: Admin) -> ApiResult {
    let removed = tokio::task::spawn_blocking(lemusic_player::cache::clear)
        .await
        .map_err(|e| ApiError::bad_request(format!("Leeren fehlgeschlagen: {e}")))?;
    Ok(Json(json!({ "removed": removed })))
}

pub(crate) async fn ytdlp_update(State(state): State<WebState>, _admin: Admin) -> ApiResult {
    info!("yt-dlp-Update über die Weboberfläche angestoßen");
    let status = ytdlp::update(&state.db, "manuell").await;
    let version = ytdlp_version(true).await;
    if !status.ok {
        return Err(ApiError::bad_request(status.message));
    }
    Ok(Json(json!({ "version": version, "status": status })))
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
