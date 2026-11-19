//! HTTP-Integrationstests: echter Router, echte Datenbank (Datei im
//! Temp-Verzeichnis), kein Discord. Geprüft werden vor allem die
//! sicherheitsrelevanten Teile – Login, CSRF, PIN-Sperre, Gast-Limits,
//! Sicherheits-Header – sowie Health, Metriken und Backup.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Once},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use lemusic_core::{Db, GuildDirectory, GuildInfo, Health, LogBuffer, NamedId, SettingsStore};
use lemusic_player::{PlayerManager, VoicePresence};
use lemusic_web::WebState;
use serde_json::{Value, json};
use tower::ServiceExt as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fake;

impl GuildDirectory for Fake {
    fn guild_info(&self, _guild_id: u64) -> Option<GuildInfo> {
        Some(GuildInfo {
            id: "1".into(),
            name: "Test".into(),
            bot_name: "LeMusic".into(),
            voice_channels: vec![NamedId {
                id: "10".into(),
                name: "Musik".into(),
            }],
            text_channels: Vec::new(),
            roles: Vec::new(),
        })
    }
    fn listeners(&self, _guild_id: u64, _channel_id: u64) -> Option<usize> {
        Some(0)
    }
}

impl VoicePresence for Fake {
    fn listeners(&self, _guild_id: u64, _channel_id: u64) -> Option<usize> {
        Some(0)
    }
}

fn data_dir() -> PathBuf {
    static INIT: Once = Once::new();
    let dir = std::env::temp_dir().join(format!("lemusic-http-tests-{}", std::process::id()));
    INIT.call_once(|| {
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: Läuft genau einmal, bevor irgendein Test die Umgebung liest
        // (alle Tests holen sich ihre App über `app()`).
        unsafe { std::env::set_var("DATA_DIR", &dir) };
    });
    dir
}

/// Eigene App je Test (eigene Datenbank, eigene Sperren).
async fn app() -> Result<(Router, WebState), Box<dyn std::error::Error>> {
    let dir = data_dir().join(format!("db-{}", fastrand::u64(..)));
    std::fs::create_dir_all(&dir)?;
    let db = Db::open(&dir.join("lemusic.db")).await?;
    db.ensure_admin(Some("admin".into()), Some("richtig-geheim".into()))
        .await?;
    let settings = SettingsStore::load(db.clone()).await?;
    let fake = Arc::new(Fake);
    let players = PlayerManager::new(
        songbird::Songbird::serenity(),
        db.clone(),
        settings.clone(),
        fake.clone(),
    );
    let state = WebState::new(
        db,
        Arc::new(Health::default()),
        players,
        settings,
        fake,
        LogBuffer::default(),
        1,
    );
    Ok((lemusic_web::router(state.clone()), state))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: impl header::AsHeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    }
    /// `name=wert` aus Set-Cookie.
    fn cookie(&self) -> String {
        self.header(header::SET_COOKIE)
            .split(';')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

/// Baut eine Anfrage wie die eigene Oberfläche (mit CSRF-Header).
struct Req {
    method: &'static str,
    path: String,
    body: Option<Value>,
    csrf: bool,
    cookie: Option<String>,
    headers: Vec<(&'static str, String)>,
    /// Absender (Verbindung); Standard: Proxy im eigenen Netz.
    peer: SocketAddr,
}

fn get(path: &str) -> Req {
    Req {
        method: "GET",
        path: path.into(),
        body: None,
        csrf: true,
        cookie: None,
        headers: Vec::new(),
        peer: SocketAddr::from(([172, 18, 0, 2], 40000)),
    }
}

fn post(path: &str, body: Value) -> Req {
    Req {
        method: "POST",
        body: Some(body),
        ..get(path)
    }
}

impl Req {
    fn no_csrf(mut self) -> Self {
        self.csrf = false;
        self
    }
    fn cookie(mut self, c: &str) -> Self {
        self.cookie = Some(c.into());
        self
    }
    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.into()));
        self
    }
    /// Gast hinter Cloudflare Tunnel mit dieser IP.
    fn guest_ip(self, ip: &str) -> Self {
        self.header("cf-connecting-ip", ip)
    }

    async fn send(self, app: &Router) -> Result<Reply, Box<dyn std::error::Error>> {
        let mut builder = Request::builder().method(self.method).uri(&self.path);
        if self.csrf {
            builder = builder.header("x-lemusic", "1");
        }
        if let Some(c) = &self.cookie {
            builder = builder.header(header::COOKIE, c);
        }
        for (name, value) in &self.headers {
            builder = builder.header(*name, value);
        }
        let body = match &self.body {
            Some(v) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(v)?)
            }
            None => Body::empty(),
        };
        let mut request = builder.body(body)?;
        request.extensions_mut().insert(ConnectInfo(self.peer));
        let response = app.clone().oneshot(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await?
            .to_vec();
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

async fn login(app: &Router) -> Result<String, Box<dyn std::error::Error>> {
    let r = post(
        "/api/login",
        json!({ "username": "admin", "password": "richtig-geheim" }),
    )
    .send(app)
    .await?;
    assert_eq!(r.status, StatusCode::OK);
    Ok(r.cookie())
}

async fn enable_public(state: &WebState, pin: &str) -> TestResult {
    let mut s = state.settings.current();
    s.public_enabled = true;
    s.public_pin = pin.into();
    state.settings.update(s).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin-Login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_needs_csrf_header() -> TestResult {
    let (app, _) = app().await?;
    let r = post(
        "/api/login",
        json!({ "username": "admin", "password": "richtig-geheim" }),
    )
    .no_csrf()
    .send(&app)
    .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn login_session_and_logout() -> TestResult {
    let (app, _) = app().await?;
    assert_eq!(
        get("/api/me").send(&app).await?.status,
        StatusCode::UNAUTHORIZED
    );
    let cookie = login(&app).await?;
    assert!(cookie.starts_with("lemusic_session="));

    let me = get("/api/me").cookie(&cookie).send(&app).await?;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["username"], "admin");

    // Schreibende Anfragen ohne CSRF-Header werden trotz Cookie abgelehnt.
    let r = post("/api/logout", json!({}))
        .cookie(&cookie)
        .no_csrf()
        .send(&app)
        .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = post("/api/logout", json!({}))
        .cookie(&cookie)
        .send(&app)
        .await?;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        get("/api/me").cookie(&cookie).send(&app).await?.status,
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

#[tokio::test]
async fn login_cookie_is_secure_behind_https_proxy() -> TestResult {
    let (app, _) = app().await?;
    let body = json!({ "username": "admin", "password": "richtig-geheim" });
    let plain = post("/api/login", body.clone()).send(&app).await?;
    assert!(!plain.header(header::SET_COOKIE).contains("Secure"));
    let https = post("/api/login", body)
        .header("x-forwarded-proto", "https")
        .send(&app)
        .await?;
    assert!(https.header(header::SET_COOKIE).ends_with("; Secure"));
    Ok(())
}

#[tokio::test]
async fn wrong_passwords_lock_the_login() -> TestResult {
    let (app, _) = app().await?;
    let wrong = json!({ "username": "admin", "password": "falsch" });
    for _ in 0..5 {
        let r = post("/api/login", wrong.clone()).send(&app).await?;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    // Gesperrt – auch das richtige Passwort wird jetzt nicht geprüft.
    let r = post(
        "/api/login",
        json!({ "username": "admin", "password": "richtig-geheim" }),
    )
    .send(&app)
    .await?;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    Ok(())
}

// ---------------------------------------------------------------------------
// Sicherheits-Header
// ---------------------------------------------------------------------------

#[tokio::test]
async fn security_headers_everywhere() -> TestResult {
    let (app, _) = app().await?;
    for path in ["/api/me", "/health", "/app.js", "/public/", "/"] {
        let r = get(path).send(&app).await?;
        assert_eq!(
            r.header(header::X_CONTENT_TYPE_OPTIONS),
            "nosniff",
            "{path}"
        );
        assert_eq!(r.header(header::X_FRAME_OPTIONS), "DENY", "{path}");
        assert!(
            r.header(header::CONTENT_SECURITY_POLICY)
                .contains("frame-ancestors 'none'"),
            "{path}"
        );
    }
    let page = get("/").send(&app).await?;
    assert!(
        page.header(header::CONTENT_SECURITY_POLICY)
            .contains("script-src 'self'")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Öffentliches Dashboard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn public_dashboard_is_off_by_default() -> TestResult {
    let (app, _) = app().await?;
    let r = get("/public/api/status").send(&app).await?;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn pin_lock_hits_only_the_guesser() -> TestResult {
    let (app, state) = app().await?;
    enable_public(&state, "4711").await?;

    let status = get("/public/api/status").send(&app).await?;
    assert_eq!(status.json()["pin_required"], true);
    assert_eq!(status.json()["unlocked"], false);
    let r = get("/public/api/player").send(&app).await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    for _ in 0..5 {
        let r = post("/public/api/unlock", json!({ "pin": "0000" }))
            .guest_ip("203.0.113.7")
            .send(&app)
            .await?;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let locked = post("/public/api/unlock", json!({ "pin": "4711" }))
        .guest_ip("203.0.113.7")
        .send(&app)
        .await?;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);

    // Ein anderer Gast kommt sofort rein.
    let ok = post("/public/api/unlock", json!({ "pin": "4711" }))
        .guest_ip("198.51.100.2")
        .send(&app)
        .await?;
    assert_eq!(ok.status, StatusCode::OK);
    let cookie = ok.cookie();
    assert!(cookie.starts_with("lemusic_guest="));
    assert!(ok.header(header::SET_COOKIE).contains("Path=/public"));

    let player = get("/public/api/player").cookie(&cookie).send(&app).await?;
    assert_eq!(player.status, StatusCode::OK);

    // PIN geändert → alte Gast-Sitzung ungültig.
    enable_public(&state, "1234").await?;
    let r = get("/public/api/player").cookie(&cookie).send(&app).await?;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn forwarded_ip_is_ignored_from_outside() -> TestResult {
    let (app, state) = app().await?;
    enable_public(&state, "4711").await?;
    // Direkt aus dem Internet: Ein wechselnder CF-Connecting-IP-Header hilft
    // nicht, die Sperre zu umgehen.
    let outside = SocketAddr::from(([198, 51, 100, 9], 5555));
    for i in 0..6 {
        let mut req = post("/public/api/unlock", json!({ "pin": "0000" }))
            .guest_ip(&format!("203.0.113.{i}"));
        req.peer = outside;
        let r = req.send(&app).await?;
        let expected = if i < 5 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(r.status, expected, "Versuch {i}");
    }
    Ok(())
}

#[tokio::test]
async fn guests_can_only_add_supported_links() -> TestResult {
    let (app, state) = app().await?;
    enable_public(&state, "").await?;

    let r = post(
        "/public/api/add",
        json!({ "links": ["https://example.org/song.mp3"] }),
    )
    .send(&app)
    .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let many: Vec<String> = (0..11)
        .map(|i| format!("https://youtu.be/{i:011}"))
        .collect();
    let r = post("/public/api/add", json!({ "links": many }))
        .send(&app)
        .await?;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = post(
        "/public/api/add",
        json!({ "links": ["https://youtu.be/x"] }),
    )
    .no_csrf()
    .send(&app)
    .await?;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    Ok(())
}

// ---------------------------------------------------------------------------
// Betrieb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_reports_every_part() -> TestResult {
    let (app, _) = app().await?;
    let r = get("/health").send(&app).await?;
    // Ohne Discord-Verbindung nicht gesund.
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let h = r.json();
    assert_eq!(h["discord"], false);
    assert_eq!(h["database"], true);
    assert_eq!(h["player"], true);
    assert_eq!(h["voice"], true);
    assert!(h["ytdlp"].is_boolean());
    Ok(())
}

#[tokio::test]
async fn metrics_in_prometheus_format() -> TestResult {
    let (app, _) = app().await?;
    let r = get("/metrics").send(&app).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header(header::CONTENT_TYPE).starts_with("text/plain"));
    let text = String::from_utf8(r.body)?;
    for name in [
        "lemusic_up 0",
        "lemusic_database_ok 1",
        "lemusic_player_ok 1",
        "# TYPE lemusic_tracks_started_total counter",
        "lemusic_track_load_seconds_count",
        "lemusic_queue_requests 0",
    ] {
        assert!(text.contains(name), "fehlt: {name}");
    }
    Ok(())
}

#[tokio::test]
async fn backup_only_for_admin_and_is_sqlite() -> TestResult {
    let (app, _) = app().await?;
    assert_eq!(
        get("/api/system/backup").send(&app).await?.status,
        StatusCode::UNAUTHORIZED
    );
    let cookie = login(&app).await?;
    let r = get("/api/system/backup").cookie(&cookie).send(&app).await?;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header(header::CONTENT_DISPOSITION)
            .starts_with("attachment; filename=\"lemusic-backup-")
    );
    assert!(r.body.starts_with(b"SQLite format 3\0"));
    // Keine Reste im Datenverzeichnis.
    let leftovers = std::fs::read_dir(data_dir())?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db.tmp"))
        .count();
    assert_eq!(leftovers, 0);
    Ok(())
}
