//! Weboberfläche ohne Discord ausprobieren (für die Entwicklung).
//!
//! Echte API und Datenbank (im Speicher) für Playlists und Einstellungen;
//! die Wiedergabe ist simuliert, damit kein zweiter Bot einem laufenden in
//! den Sprachkanal funkt.
//!
//! ```bash
//! cargo run -p lemusic-web --example ui_demo   # http://127.0.0.1:8099, admin / demo-passwort
//! ```

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use lemusic_core::{
    Db, GuildDirectory, GuildInfo, Health, LogBuffer, NamedId, SettingsStore, TrackMeta, track_key,
};
use lemusic_player::{PlayerManager, VoicePresence};
use lemusic_web::WebState;
use serde_json::{Value, json};

struct Fake;

impl GuildDirectory for Fake {
    fn guild_info(&self, _guild_id: u64) -> Option<GuildInfo> {
        let named = |id: &str, name: &str| NamedId {
            id: id.into(),
            name: name.into(),
        };
        Some(GuildInfo {
            id: "1".into(),
            name: "Demo-Server".into(),
            bot_name: "LeMusic".into(),
            voice_channels: vec![named("10", "Musik"), named("11", "Lounge")],
            text_channels: vec![named("20", "bot")],
            roles: vec![named("30", "DJ")],
        })
    }
    fn listeners(&self, _guild_id: u64, _channel_id: u64) -> Option<usize> {
        Some(2)
    }
}

impl VoicePresence for Fake {
    fn listeners(&self, _guild_id: u64, _channel_id: u64) -> Option<usize> {
        Some(2)
    }
}

fn track(i: usize) -> TrackMeta {
    TrackMeta {
        title: format!("Demo-Titel {i:03}"),
        author: format!("Interpret {}", (i * 7) % 23),
        url: format!("https://www.youtube.com/watch?v=demo{i:04}"),
        duration: Some(std::time::Duration::from_secs(120 + (i as u64 * 37) % 240)),
        thumbnail: None,
    }
}

fn track_json(t: &TrackMeta) -> Value {
    json!({
        "title": t.title, "author": t.author, "url": t.url,
        "duration": t.duration.map(|d| d.as_secs()), "thumbnail": t.thumbnail,
    })
}

#[derive(Default)]
struct Sim {
    next_id: u64,
    now: Option<Value>,
    queue: Vec<Value>,
    trash: Vec<(usize, Value)>,
}

type SimState = Arc<Mutex<Sim>>;

impl Sim {
    fn entry(&mut self, track: Value, autoplay: bool) -> Value {
        self.next_id += 1;
        json!({ "id": self.next_id, "track": track, "requester": null, "autoplay": autoplay })
    }
    fn id(v: &Value) -> u64 {
        v["id"].as_u64().unwrap_or_default()
    }
    fn take(&mut self, mut remove: impl FnMut(&Value) -> bool) -> usize {
        let mut removed = Vec::new();
        let mut kept = Vec::new();
        for (i, e) in self.queue.drain(..).enumerate() {
            if remove(&e) {
                removed.push((i, e));
            } else {
                kept.push(e);
            }
        }
        self.queue = kept;
        let n = removed.len();
        if n > 0 {
            self.trash = removed;
        }
        n
    }
}

fn lock(sim: &SimState) -> std::sync::MutexGuard<'_, Sim> {
    match sim.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

async fn player(State(sim): State<SimState>) -> Json<Value> {
    let s = lock(&sim);
    let dur: u64 = s
        .queue
        .iter()
        .filter_map(|e| e["track"]["duration"].as_u64())
        .sum();
    Json(json!({
        "voice_channel": "10", "listeners": 2,
        "now": s.now.as_ref().map(|e| json!({ "entry": e, "position": 42, "paused": false, "ready": true })),
        "queue": s.queue, "queue_duration": dur, "repeat": "off", "volume": 100,
    }))
}

async fn edit(State(sim): State<SimState>, Json(body): Json<Value>) -> Json<Value> {
    let mut s = lock(&sim);
    let ids: HashSet<u64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let affected = match body["op"].as_str().unwrap_or_default() {
        "remove" => s.take(|e| ids.contains(&Sim::id(e))),
        "move" => {
            let (moved, mut rest): (Vec<_>, Vec<_>) =
                s.queue.drain(..).partition(|e| ids.contains(&Sim::id(e)));
            let at = if body["front"].as_bool().unwrap_or(false) {
                0
            } else {
                match body["before"].as_u64() {
                    Some(b) => rest
                        .iter()
                        .position(|e| Sim::id(e) == b)
                        .unwrap_or(rest.len()),
                    None => rest.len(),
                }
            };
            let n = moved.len();
            rest.splice(at..at, moved);
            s.queue = rest;
            n
        }
        "restore" => {
            let trash = std::mem::take(&mut s.trash);
            let n = trash.len();
            for (i, e) in trash {
                let i = i.min(s.queue.len());
                s.queue.insert(i, e);
            }
            n
        }
        "dedupe" => {
            let mut seen = HashSet::new();
            s.take(|e| !seen.insert(track_key(e["track"]["url"].as_str().unwrap_or_default())))
        }
        "remove_autoplay" => s.take(|e| e["autoplay"].as_bool().unwrap_or(false)),
        _ => 0,
    };
    Json(json!({ "affected": affected }))
}

async fn enqueue(State(sim): State<SimState>, Json(body): Json<Value>) -> Json<Value> {
    let mut s = lock(&sim);
    let tracks = body["tracks"].as_array().cloned().unwrap_or_default();
    let entries: Vec<Value> = tracks.into_iter().map(|t| s.entry(t, false)).collect();
    let n = entries.len();
    let at = match body["before"].as_u64() {
        Some(b) => s
            .queue
            .iter()
            .position(|e| Sim::id(e) == b)
            .unwrap_or(s.queue.len()),
        None if body["next"].as_bool().unwrap_or(false)
            || body["now"].as_bool().unwrap_or(false) =>
        {
            0
        }
        None => s
            .queue
            .iter()
            .position(|e| e["autoplay"].as_bool().unwrap_or(false))
            .unwrap_or(s.queue.len()),
    };
    s.queue.splice(at..at, entries);
    if body["now"].as_bool().unwrap_or(false) && !s.queue.is_empty() {
        s.now = Some(s.queue.remove(0));
    }
    Json(json!({ "added": n, "position": at + 1, "skipped_too_long": 0 }))
}

async fn action(
    State(sim): State<SimState>,
    Path(action): Path<String>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    let mut s = lock(&sim);
    match action.as_str() {
        "skip" => {
            s.now = (!s.queue.is_empty()).then(|| s.queue.remove(0));
        }
        "clear" => {
            s.take(|_| true);
        }
        "shuffle" => fastrand::shuffle(&mut s.queue),
        "skipto" => {
            let i = body
                .and_then(|Json(b)| b["index"].as_u64())
                .unwrap_or_default() as usize;
            if i < s.queue.len() {
                s.queue.drain(..i);
                s.now = Some(s.queue.remove(0));
            }
        }
        _ => {}
    }
    Json(json!({ "ok": true }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = Db::in_memory().await?;
    db.ensure_admin(Some("admin".into()), Some("demo-passwort".into()))
        .await?;
    let big: Vec<TrackMeta> = (1..=300).map(track).collect();
    db.playlist_save("Große Playlist", &big).await?;
    let mut dupes: Vec<TrackMeta> = (1..=8).map(track).collect();
    dupes.extend((1..=3).map(track));
    db.playlist_save("Mit Duplikaten", &dupes).await?;
    let real = [
        (
            "Rick Astley - Never Gonna Give You Up",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        ),
        (
            "Gelöschtes Video",
            "https://www.youtube.com/watch?v=aaaaaaaaaaa",
        ),
        ("Privat", "https://www.youtube.com/watch?v=Zm7lGk6bnkM"),
    ];
    let real: Vec<TrackMeta> = real
        .iter()
        .map(|(title, url)| TrackMeta {
            title: (*title).into(),
            author: "YouTube".into(),
            url: (*url).into(),
            duration: None,
            thumbnail: None,
        })
        .collect();
    db.playlist_save("Echte Links", &real).await?;
    db.playlist_save("Party", &big[..12]).await?;

    let settings = SettingsStore::load(db.clone()).await?;
    lemusic_player::cache::watch_settings(settings.clone());
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

    let sim: SimState = Arc::default();
    {
        let mut s = lock(&sim);
        let first = s.entry(track_json(&track(500)), false);
        s.now = Some(first);
        for i in 501..=506 {
            let e = s.entry(track_json(&track(i)), false);
            s.queue.push(e);
        }
        for i in 1..=40 {
            let e = s.entry(track_json(&track(i)), true);
            s.queue.push(e);
        }
    }
    let fake_player = Router::new()
        .route("/api/player", get(player))
        .route("/api/player/queue", post(edit))
        .route("/api/player/enqueue", post(enqueue))
        .route("/api/player/{action}", post(action))
        .with_state(sim);
    let app = fake_player.fallback_service(lemusic_web::router(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8099").await?;
    println!("UI-Demo: http://127.0.0.1:8099  (admin / demo-passwort)");
    axum::serve(listener, app).await?;
    Ok(())
}
