//! Öffentliches Dashboard (`/public`): Wiedergabe ansehen und YouTube-/
//! SoundCloud-Links hinzufügen – ohne Admin-Login, optional mit 4-stelliger
//! PIN.
//!
//! Gäste bekommen nach der PIN-Eingabe ein eigenes Cookie. Die Sitzungen
//! liegen nur im Speicher (nach einem Neustart PIN erneut eingeben) und
//! werden ungültig, sobald der Admin die PIN ändert.
//!
//! Schutz gegen PIN-Raten ([`PinGuard`]): Fehlversuche zählen pro
//! Absender-IP mit wachsender Sperre, zusätzlich gibt es eine Obergrenze für
//! alle Absender zusammen. Ein Angreifer sperrt so nur sich selbst aus, nicht
//! die anderen Gäste.

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
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
    auth::{cookie_value, has_csrf, has_csrf_header, is_https, named_cookie_header},
    dto::PlayerDto,
};

const COOKIE: &str = "lemusic_guest";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// So viele Links dürfen Gäste insgesamt pro Minute hinzufügen.
const LINKS_PER_MINUTE: usize = 30;
/// So viele Titel dürfen Gäste insgesamt pro Minute einreihen (eine
/// Playlist zählt mit all ihren Titeln). Wie viele Gast-Wünsche gleichzeitig
/// warten dürfen, begrenzt der Player ([`lemusic_player::MAX_GUEST_QUEUED`]).
const TRACKS_PER_MINUTE: usize = 100;
/// Höchstens so viele Links auf einmal (z. B. beim Hineinziehen mehrerer).
const MAX_LINKS_PER_REQUEST: usize = 10;

/// So viele Fehlversuche pro Absender sind ohne Sperre erlaubt.
const PIN_FREE_ATTEMPTS: u32 = 5;
/// Erste Sperre danach; verdoppelt sich mit jedem weiteren Fehlversuch.
const PIN_FIRST_LOCK: Duration = Duration::from_secs(60);
const PIN_MAX_LOCK: Duration = Duration::from_secs(3600);
/// Fehlversuche eines Absenders werden so lange gemerkt.
const PIN_MEMORY: Duration = Duration::from_secs(24 * 3600);
/// Obergrenze für alle Absender zusammen (z. B. viele IPs eines Botnetzes):
/// so viele Fehlversuche pro [`PIN_MEMORY`], danach keine neuen Anmeldungen
/// mehr, bis der Admin die PIN ändert. Laufende Sitzungen bleiben gültig.
const PIN_GLOBAL_MAX_FAILURES: usize = 300;

struct GuestSession {
    expires: Instant,
    /// PIN zum Zeitpunkt der Anmeldung – ändert der Admin sie, ist die
    /// Sitzung ungültig.
    pin: String,
}

/// Gleitendes Zeitfenster von einer Minute für Mengenbegrenzungen.
#[derive(Default)]
struct MinuteWindow(Mutex<VecDeque<Instant>>);

impl MinuteWindow {
    /// Gewährt bis zu `want` Einheiten, höchstens `limit` pro Minute.
    /// `partial`: auch weniger als gewünscht gewähren (sonst alles oder
    /// nichts). Liefert die gewährte Anzahl.
    fn take(&self, want: usize, limit: usize, partial: bool) -> usize {
        let Ok(mut events) = self.0.lock() else {
            return 0;
        };
        while events
            .front()
            .is_some_and(|t| t.elapsed() > Duration::from_secs(60))
        {
            events.pop_front();
        }
        let free = limit.saturating_sub(events.len());
        let granted = match (want <= free, partial) {
            (true, _) => want,
            (false, true) => free,
            (false, false) => 0,
        };
        events.extend(std::iter::repeat_n(Instant::now(), granted));
        granted
    }
}

/// Fehlversuche eines Absenders.
struct Strikes {
    failures: u32,
    last: Instant,
    locked_until: Option<Instant>,
}

/// Warum gerade keine PIN geprüft wird.
#[derive(Debug, PartialEq, Eq)]
enum PinLock {
    /// Dieser Absender hat zu oft falsch geraten.
    Sender(Duration),
    /// Insgesamt zu viele Fehlversuche – erst nach einer PIN-Änderung wieder.
    Global,
}

/// Bremse gegen PIN-Raten, siehe Moduldoku.
#[derive(Default)]
struct PinGuard {
    inner: Mutex<PinGuardInner>,
}

#[derive(Default)]
struct PinGuardInner {
    /// PIN, für die die Zähler gelten – nach einer Änderung fängt alles neu an.
    pin: String,
    senders: HashMap<IpAddr, Strikes>,
    global: VecDeque<Instant>,
    global_warned: bool,
}

impl PinGuardInner {
    fn prepare(&mut self, pin: &str) {
        if self.pin != pin {
            *self = Self {
                pin: pin.to_string(),
                ..Self::default()
            };
        }
        self.senders.retain(|_, s| s.last.elapsed() < PIN_MEMORY);
        while self
            .global
            .front()
            .is_some_and(|t| t.elapsed() >= PIN_MEMORY)
        {
            self.global.pop_front();
        }
    }
}

/// Sperrdauer nach dem `failures`-ten Fehlversuch.
fn pin_lock_duration(failures: u32) -> Option<Duration> {
    let over = failures.checked_sub(PIN_FREE_ATTEMPTS)?;
    let factor = 1u32.checked_shl(over).unwrap_or(u32::MAX);
    Some(PIN_FIRST_LOCK.saturating_mul(factor).min(PIN_MAX_LOCK))
}

impl PinGuard {
    fn locked(&self, sender: IpAddr, pin: &str) -> Option<PinLock> {
        let Ok(mut inner) = self.inner.lock() else {
            return Some(PinLock::Global);
        };
        inner.prepare(pin);
        if inner.global.len() >= PIN_GLOBAL_MAX_FAILURES {
            return Some(PinLock::Global);
        }
        let until = inner.senders.get(&sender)?.locked_until?;
        let wait = until.checked_duration_since(Instant::now())?;
        Some(PinLock::Sender(wait))
    }

    fn record_failure(&self, sender: IpAddr, pin: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.prepare(pin);
        let now = Instant::now();
        inner.global.push_back(now);
        let strikes = inner.senders.entry(sender).or_insert(Strikes {
            failures: 0,
            last: now,
            locked_until: None,
        });
        strikes.failures = strikes.failures.saturating_add(1);
        strikes.last = now;
        strikes.locked_until = pin_lock_duration(strikes.failures).map(|d| now + d);
        let failures = strikes.failures;
        warn!(%sender, failures, "Falsche PIN am öffentlichen Dashboard");
        if inner.global.len() >= PIN_GLOBAL_MAX_FAILURES && !inner.global_warned {
            inner.global_warned = true;
            warn!(
                "Sehr viele falsche PINs von verschiedenen Absendern – neue Anmeldungen am \
                 öffentlichen Dashboard sind gesperrt, bis die PIN geändert wird"
            );
        }
    }

    fn record_success(&self, sender: IpAddr) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.senders.remove(&sender);
        }
    }
}

/// Zeitlich konstanter Vergleich (verrät nicht, wie viele Stellen stimmen).
fn pin_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Absender einer Anfrage, nach dem die PIN-Bremse zählt.
///
/// Kommt die Verbindung von einem Proxy im eigenen Netz (Cloudflare Tunnel,
/// Reverse Proxy, Docker-Netz), gilt die Adresse, die er weiterreicht
/// (`CF-Connecting-IP`, `X-Real-IP`, letzter Eintrag von `X-Forwarded-For`).
/// Direkten Verbindungen von außen wird kein Header geglaubt. IPv6 zählt pro
/// /64-Netz, weil ein Anschluss meist ein ganzes solches Netz hat.
pub(crate) struct Sender(IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for Sender {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(Sender(sender_ip(peer, &parts.headers)))
    }
}

fn sender_ip(peer: Option<IpAddr>, headers: &HeaderMap) -> IpAddr {
    let peer = peer.map(canonical_ip);
    let forwarded = || {
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        header("cf-connecting-ip")
            .or_else(|| header("x-real-ip"))
            .or_else(|| header("x-forwarded-for").and_then(|v| v.rsplit(',').next()))
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    };
    let ip = match peer {
        Some(p) if is_local(p) => forwarded().unwrap_or(p),
        Some(p) => p,
        None => forwarded().unwrap_or(IpAddr::from([0, 0, 0, 0])),
    };
    match canonical_ip(ip) {
        IpAddr::V6(v6) => {
            let mut seg = v6.segments();
            seg[4..].fill(0);
            IpAddr::V6(Ipv6Addr::from(seg))
        }
        v4 => v4,
    }
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Loopback oder privates Netz (dort sitzt ein Proxy, dem wir glauben).
fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

#[derive(Default)]
pub(crate) struct PublicState {
    sessions: Mutex<HashMap<String, GuestSession>>,
    links: MinuteWindow,
    tracks: MinuteWindow,
    pins: PinGuard,
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

    /// Zählt Links; `false`, wenn das Limit erreicht ist (alles oder nichts).
    fn allow_links(&self, count: usize) -> bool {
        self.links.take(count, LINKS_PER_MINUTE, false) == count
    }

    /// Wie viele der `count` Titel noch ins Minutenlimit passen.
    fn take_tracks(&self, count: usize) -> usize {
        self.tracks.take(count, TRACKS_PER_MINUTE, true)
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
    Sender(sender): Sender,
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
    let pin = &settings.public_pin;
    match state.public.pins.locked(sender, pin) {
        Some(PinLock::Sender(wait)) => {
            return Err(ApiError::too_many(&format!(
                "Zu viele falsche Versuche – bitte {} warten",
                wait_label(wait)
            )));
        }
        Some(PinLock::Global) => {
            return Err(ApiError::too_many(
                "Anmelden ist gerade gesperrt – bitte den Admin fragen",
            ));
        }
        None => {}
    }
    if pin.is_empty() || pin_matches(form.pin.trim(), pin) {
        state.public.pins.record_success(sender);
        let token = state
            .public
            .create_session(pin)
            .ok_or_else(|| ApiError::bad_request("Interner Fehler"))?;
        let secure = is_https(&headers, &settings.public_url);
        let mut response = Json(json!({ "ok": true })).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            named_cookie_header(COOKIE, &token, SESSION_TTL.as_secs(), "/public", secure),
        );
        return Ok(response);
    }
    state.public.pins.record_failure(sender, pin);
    tokio::time::sleep(Duration::from_millis(500)).await;
    Err(ApiError::unauthorized("Falsche PIN"))
}

/// „45 s“, „3 min“
fn wait_label(wait: Duration) -> String {
    let secs = wait.as_secs().max(1);
    if secs < 90 {
        format!("{secs} s")
    } else {
        format!("{} min", secs.div_ceil(60))
    }
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
    for entry in dto.queue.iter_mut().chain(dto.history.iter_mut()) {
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

/// Anonyme Kennung eines Gasts für das faire Mischen: aus seiner Adresse
/// (IPv6 pro /64) abgeleitet, nie 0. Die Adresse selbst wird nicht
/// gespeichert.
fn guest_id(sender: IpAddr) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sender.hash(&mut hasher);
    hasher.finish().max(1)
}

/// Fügt YouTube-/SoundCloud-Links hinzu (auch Playlists, gekürzt).
pub(crate) async fn add(
    State(state): State<WebState>,
    _guest: Guest,
    Sender(sender): Sender,
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
    if !state.public.allow_links(links.len()) {
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
    if tracks.is_empty() {
        return Err(ApiError::bad_request(
            failed
                .first()
                .and_then(|f| f["error"].as_str())
                .unwrap_or("Nichts gefunden")
                .to_string(),
        ));
    }
    let granted = state.public.take_tracks(tracks.len());
    if granted == 0 {
        return Err(ApiError::too_many(
            "Gerade wurden viele Titel gewünscht – bitte kurz warten",
        ));
    }
    let truncated = granted < tracks.len();
    tracks.truncate(granted);

    let count = tracks.len();
    let Json(result) =
        enqueue(&state, tracks, channel, false, None, Some(guest_id(sender))).await?;
    info!(
        links = links.len(),
        tracks = count,
        "Über das öffentliche Dashboard hinzugefügt"
    );
    Ok(Json(json!({
        "added": result["added"],
        "position": result["position"],
        "skipped_too_long": result["skipped_too_long"],
        "queue_full": result["queue_full"],
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
        assert!(state.allow_links(LINKS_PER_MINUTE - 1));
        assert!(!state.allow_links(2));
        assert!(state.allow_links(1));
        assert!(!state.allow_links(1));
    }

    #[test]
    fn rate_limit_counts_tracks_partially() {
        let state = PublicState::default();
        assert_eq!(
            state.take_tracks(TRACKS_PER_MINUTE - 10),
            TRACKS_PER_MINUTE - 10
        );
        // Eine große Playlist bekommt nur den Rest.
        assert_eq!(state.take_tracks(200), 10);
        assert_eq!(state.take_tracks(1), 0);
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn pin_lock_grows_and_is_capped() {
        assert_eq!(pin_lock_duration(PIN_FREE_ATTEMPTS - 1), None);
        assert_eq!(pin_lock_duration(PIN_FREE_ATTEMPTS), Some(PIN_FIRST_LOCK));
        assert_eq!(
            pin_lock_duration(PIN_FREE_ATTEMPTS + 2),
            Some(PIN_FIRST_LOCK * 4)
        );
        assert_eq!(
            pin_lock_duration(PIN_FREE_ATTEMPTS + 40),
            Some(PIN_MAX_LOCK)
        );
        assert_eq!(pin_lock_duration(u32::MAX), Some(PIN_MAX_LOCK));
    }

    #[test]
    fn pin_guard_locks_only_the_guesser() {
        let guard = PinGuard::default();
        let (attacker, guest) = (ip("203.0.113.7"), ip("198.51.100.2"));
        for _ in 0..PIN_FREE_ATTEMPTS - 1 {
            guard.record_failure(attacker, "1234");
            assert_eq!(guard.locked(attacker, "1234"), None);
        }
        guard.record_failure(attacker, "1234");
        assert!(matches!(
            guard.locked(attacker, "1234"),
            Some(PinLock::Sender(_))
        ));
        // Andere Gäste sind nicht betroffen.
        assert_eq!(guard.locked(guest, "1234"), None);
        // Neue PIN → Zähler fangen neu an.
        assert_eq!(guard.locked(attacker, "5678"), None);
    }

    #[test]
    fn pin_guard_has_global_ceiling() {
        let guard = PinGuard::default();
        for i in 0..PIN_GLOBAL_MAX_FAILURES {
            let sender = IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]);
            guard.record_failure(sender, "1234");
        }
        assert_eq!(guard.locked(ip("192.0.2.1"), "1234"), Some(PinLock::Global));
        // PIN-Änderung durch den Admin hebt die Sperre auf.
        assert_eq!(guard.locked(ip("192.0.2.1"), "4321"), None);
    }

    #[test]
    fn success_clears_sender_strikes() {
        let guard = PinGuard::default();
        let sender = ip("203.0.113.7");
        for _ in 0..PIN_FREE_ATTEMPTS - 1 {
            guard.record_failure(sender, "1234");
        }
        guard.record_success(sender);
        guard.record_failure(sender, "1234");
        assert_eq!(guard.locked(sender, "1234"), None);
    }

    #[test]
    fn guest_ids_are_stable_and_distinct() {
        let a = guest_id(ip("203.0.113.7"));
        assert_eq!(a, guest_id(ip("203.0.113.7")));
        assert_ne!(a, guest_id(ip("203.0.113.8")));
        assert_ne!(a, 0);
    }

    #[test]
    fn pin_compare() {
        assert!(pin_matches("1234", "1234"));
        assert!(!pin_matches("1235", "1234"));
        assert!(!pin_matches("123", "1234"));
        assert!(!pin_matches("12345", "1234"));
    }

    #[test]
    fn sender_from_trusted_proxy_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cf-connecting-ip",
            axum::http::HeaderValue::from_static("203.0.113.9"),
        );
        // Über einen Proxy im eigenen Netz (z. B. cloudflared im Docker-Netz).
        assert_eq!(
            sender_ip(Some(ip("172.18.0.3")), &headers),
            ip("203.0.113.9")
        );
        assert_eq!(
            sender_ip(Some(ip("127.0.0.1")), &headers),
            ip("203.0.113.9")
        );
        // Direkt von außen: Header wird ignoriert.
        assert_eq!(
            sender_ip(Some(ip("198.51.100.4")), &headers),
            ip("198.51.100.4")
        );

        // X-Forwarded-For: der letzte Eintrag stammt vom Proxy selbst.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("1.2.3.4, 203.0.113.10"),
        );
        assert_eq!(
            sender_ip(Some(ip("10.0.0.2")), &headers),
            ip("203.0.113.10")
        );

        // IPv6 zählt pro /64, IPv4-mapped als IPv4.
        let none = HeaderMap::new();
        assert_eq!(
            sender_ip(Some(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")), &none),
            ip("2001:db8:1:2::")
        );
        assert_eq!(
            sender_ip(Some(ip("::ffff:198.51.100.4")), &none),
            ip("198.51.100.4")
        );
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
