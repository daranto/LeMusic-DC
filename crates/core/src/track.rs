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
}
