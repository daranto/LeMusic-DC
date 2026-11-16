//! Metadaten eines Tracks – gemeinsam für Player, Playlists und Weboberfläche.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMeta {
    pub title: String,
    pub author: String,
    /// Seiten-URL (YouTube/SoundCloud), nicht die kurzlebige Stream-URL.
    pub url: String,
    /// `None` bei Livestreams oder unbekannter Länge.
    pub duration: Option<Duration>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl TrackMeta {
    /// Schlüssel zum Erkennen doppelter Titel (gleiches Video unter
    /// verschiedenen Link-Formen).
    pub fn key(&self) -> String {
        track_key(&self.url)
    }
}

/// Normalisiert einen YouTube-/SoundCloud-Link auf einen Vergleichsschlüssel:
/// YouTube → `yt:<video-id>`, sonst Host + Pfad ohne Query, klein geschrieben.
pub fn track_key(url: &str) -> String {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))
        .unwrap_or(url.trim());
    let (host_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let host_path = host_path.split('#').next().unwrap_or_default();
    let (host, path) = host_path.split_once('/').unwrap_or((host_path, ""));
    let host = host.to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(&host);
    let video_id = match host {
        "youtu.be" => path.split('/').next().map(str::to_string),
        "youtube.com" | "music.youtube.com" => {
            if let Some(id) = path
                .strip_prefix("shorts/")
                .or_else(|| path.strip_prefix("live/"))
            {
                id.split('/').next().map(str::to_string)
            } else {
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("v="))
                    .map(str::to_string)
            }
        }
        _ => None,
    };
    match video_id.filter(|id| !id.is_empty()) {
        Some(id) => format!("yt:{id}"),
        None => format!("{host}/{}", path.trim_end_matches('/')).to_ascii_lowercase(),
    }
}

/// Formatiert eine Dauer als `m:ss` bzw. `h:mm:ss`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Liest `1:23`, `01:02:03` oder `95` (Sekunden).
pub fn parse_duration(input: &str) -> Option<Duration> {
    let mut total: u64 = 0;
    let parts: Vec<&str> = input.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in parts {
        let value: u64 = part.trim().parse().ok()?;
        total = total.checked_mul(60)?.checked_add(value)?;
    }
    Some(Duration::from_secs(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_roundtrip() {
        assert_eq!(format_duration(Duration::from_secs(65)), "1:05");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(parse_duration("1:05"), Some(Duration::from_secs(65)));
        assert_eq!(parse_duration("1:02:05"), Some(Duration::from_secs(3725)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("a:b"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
    }

    #[test]
    fn track_keys_match_link_variants() {
        let k = track_key("https://www.youtube.com/watch?v=abc123&list=PL1&index=4");
        assert_eq!(k, "yt:abc123");
        assert_eq!(track_key("https://youtu.be/abc123?si=xyz"), k);
        assert_eq!(track_key("https://music.youtube.com/watch?v=abc123"), k);
        assert_eq!(track_key("https://m.youtube.com/shorts/abc123"), k);
        assert_eq!(
            track_key("https://soundcloud.com/Artist/Track/?utm=1"),
            track_key("https://m.soundcloud.com/artist/track")
        );
        assert_ne!(
            track_key("https://youtu.be/a"),
            track_key("https://youtu.be/b")
        );
    }
}
