//! JSON-Formen für den Browser. Discord-IDs als Strings (JavaScript kann
//! 64-Bit-Zahlen nicht exakt darstellen).

use anyhow::{Context as _, Result};
use lemusic_core::{GuildSettings, TrackMeta};
use lemusic_player::{QueueEntry, RepeatMode, Snapshot};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct TrackDto {
    pub title: String,
    pub author: String,
    pub url: String,
    pub duration: Option<u64>,
    pub thumbnail: Option<String>,
}

impl From<&TrackMeta> for TrackDto {
    fn from(m: &TrackMeta) -> Self {
        Self {
            title: m.title.clone(),
            author: m.author.clone(),
            url: m.url.clone(),
            duration: m.duration.map(|d| d.as_secs()),
            thumbnail: m.thumbnail.clone(),
        }
    }
}

/// Titel, wie ihn der Browser zurückschickt (Kopieren, Einfügen, Import).
#[derive(Deserialize)]
pub struct TrackInput {
    pub title: String,
    #[serde(default)]
    pub author: String,
    pub url: String,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

fn is_http(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

impl TrackInput {
    pub fn into_meta(self) -> Result<TrackMeta> {
        let url = self.url.trim().to_string();
        if !is_http(&url) || url.len() > 2048 {
            anyhow::bail!("Ungültiger Link: {url}");
        }
        let clip = |s: String, n: usize| s.trim().chars().take(n).collect::<String>();
        let title = clip(self.title, 300);
        Ok(TrackMeta {
            title: if title.is_empty() { url.clone() } else { title },
            author: clip(self.author, 200),
            url,
            duration: self.duration.map(std::time::Duration::from_secs),
            thumbnail: self
                .thumbnail
                .map(|t| t.trim().to_string())
                .filter(|t| is_http(t) && t.len() <= 2048),
        })
    }

    pub fn into_metas(list: Vec<Self>) -> Result<Vec<TrackMeta>> {
        list.into_iter().map(Self::into_meta).collect()
    }
}

#[derive(Serialize)]
pub struct EntryDto {
    pub id: u64,
    pub track: TrackDto,
    pub requester: Option<String>,
    pub autoplay: bool,
    /// Über das öffentliche Dashboard gewünscht.
    pub guest: bool,
}

impl From<&QueueEntry> for EntryDto {
    fn from(e: &QueueEntry) -> Self {
        Self {
            id: e.id,
            track: TrackDto::from(&*e.meta),
            requester: (e.requester != 0).then(|| e.requester.to_string()),
            autoplay: e.autoplay,
            guest: e.guest,
        }
    }
}

#[derive(Serialize)]
pub struct NowDto {
    pub entry: EntryDto,
    pub position: u64,
    pub paused: bool,
    pub ready: bool,
}

#[derive(Serialize)]
pub struct PlayerDto {
    pub voice_channel: Option<String>,
    pub listeners: Option<usize>,
    pub now: Option<NowDto>,
    pub queue: Vec<EntryDto>,
    /// Zuletzt gespielt, der jüngste zuerst.
    pub history: Vec<EntryDto>,
    pub queue_duration: u64,
    pub repeat: RepeatMode,
    pub volume: u16,
}

impl PlayerDto {
    pub fn new(s: &Snapshot, listeners: Option<usize>) -> Self {
        Self {
            voice_channel: s.voice_channel.map(|c| c.to_string()),
            listeners,
            now: s.now.as_ref().map(|n| NowDto {
                entry: EntryDto::from(&n.entry),
                position: n.position.as_secs(),
                paused: n.paused,
                ready: n.ready,
            }),
            queue: s.queue.iter().map(EntryDto::from).collect(),
            history: s.history.iter().map(EntryDto::from).collect(),
            queue_duration: s.queue_duration().as_secs(),
            repeat: s.repeat,
            volume: s.volume,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct GuildSettingsDto {
    pub dj_role: Option<String>,
    pub text_channel: Option<String>,
    pub voice_channel: Option<String>,
    /// Prozent (0–100).
    pub skip_ratio: u8,
    pub max_track_secs: u64,
    pub autoplaylist: Option<String>,
    pub announce_tracks: bool,
    #[serde(default)]
    pub announce_channel: Option<String>,
    #[serde(default)]
    pub auto_join: bool,
    #[serde(default)]
    pub auto_join_channels: Vec<String>,
}

fn id_to_string(id: Option<u64>) -> Option<String> {
    id.map(|i| i.to_string())
}

fn parse_id(id: Option<String>, what: &str) -> Result<Option<u64>> {
    id.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().with_context(|| format!("Ungültige {what}")))
        .transpose()
}

impl From<&GuildSettings> for GuildSettingsDto {
    fn from(s: &GuildSettings) -> Self {
        Self {
            dj_role: id_to_string(s.dj_role),
            text_channel: id_to_string(s.text_channel),
            voice_channel: id_to_string(s.voice_channel),
            skip_ratio: (s.skip_ratio * 100.0).round().clamp(0.0, 100.0) as u8,
            max_track_secs: s.max_track_secs,
            autoplaylist: s.autoplaylist.clone(),
            announce_tracks: s.announce_tracks,
            announce_channel: id_to_string(s.announce_channel),
            auto_join: s.auto_join,
            auto_join_channels: s.auto_join_channels.iter().map(u64::to_string).collect(),
        }
    }
}

impl GuildSettingsDto {
    pub fn into_settings(self) -> Result<GuildSettings> {
        Ok(GuildSettings {
            dj_role: parse_id(self.dj_role, "Rolle")?,
            text_channel: parse_id(self.text_channel, "Textkanal-ID")?,
            voice_channel: parse_id(self.voice_channel, "Sprachkanal-ID")?,
            skip_ratio: f32::from(self.skip_ratio.min(100)) / 100.0,
            max_track_secs: self.max_track_secs,
            autoplaylist: self
                .autoplaylist
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            announce_tracks: self.announce_tracks,
            announce_channel: parse_id(self.announce_channel, "Ansage-Kanal-ID")?,
            auto_join: self.auto_join,
            auto_join_channels: self
                .auto_join_channels
                .into_iter()
                .filter_map(|id| parse_id(Some(id), "Sprachkanal-ID").transpose())
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_input_is_validated() {
        let ok = TrackInput {
            title: " T ".into(),
            author: String::new(),
            url: "https://youtu.be/abc".into(),
            duration: Some(61),
            thumbnail: Some("javascript:alert(1)".into()),
        }
        .into_meta();
        let meta = ok.ok();
        assert_eq!(meta.as_ref().map(|m| m.title.as_str()), Some("T"));
        assert_eq!(meta.and_then(|m| m.thumbnail), None);
        let bad = TrackInput {
            title: "x".into(),
            author: String::new(),
            url: "file:///etc/passwd".into(),
            duration: None,
            thumbnail: None,
        };
        assert!(bad.into_meta().is_err());
    }

    #[test]
    fn guild_settings_roundtrip_keeps_big_ids() -> Result<()> {
        let original = GuildSettings {
            dj_role: Some(123_456_789_012_345_678),
            skip_ratio: 0.55,
            ..GuildSettings::default()
        };
        let dto = GuildSettingsDto::from(&original);
        assert_eq!(dto.dj_role.as_deref(), Some("123456789012345678"));
        assert_eq!(dto.skip_ratio, 55);
        let back = dto.into_settings()?;
        assert_eq!(back.dj_role, original.dj_role);
        assert!((back.skip_ratio - 0.55).abs() < 0.001);
        Ok(())
    }
}
