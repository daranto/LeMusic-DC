//! Auflösen von Links und Suchanfragen über yt-dlp.
//!
//! Hier werden nur Metadaten geholt (`--flat-playlist`). Die eigentliche
//! Stream-URL wird erst beim Abspielen ermittelt, weil sie nach einigen
//! Stunden abläuft.

use std::{process::Stdio, time::Duration};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

/// Maximale Laufzeit eines yt-dlp-Aufrufs.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Obergrenze für Playlists, damit niemand 5000 Tracks einreiht.
pub const MAX_PLAYLIST_TRACKS: usize = 200;

#[derive(Debug, Clone, Copy)]
pub enum SearchSource {
    YouTube,
    SoundCloud,
}

/// Metadaten eines Tracks in der Warteschlange.
#[derive(Debug, Clone)]
pub struct TrackMeta {
    pub title: String,
    pub author: String,
    pub url: String,
    pub duration: Option<Duration>,
}

#[derive(Debug, Deserialize)]
struct YtDlpItem {
    #[serde(rename = "_type")]
    kind: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    entries: Vec<YtDlpItem>,
}

impl YtDlpItem {
    fn into_meta(self) -> Option<TrackMeta> {
        let url = self.webpage_url.or(self.url)?;
        Some(TrackMeta {
            title: self.title.unwrap_or_else(|| url.clone()),
            author: self.uploader.or(self.channel).unwrap_or_default(),
            url,
            duration: self
                .duration
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(Duration::from_secs_f64),
        })
    }
}

pub fn is_url(query: &str) -> bool {
    query.starts_with("https://") || query.starts_with("http://")
}

/// Löst einen Link (Track oder Playlist) oder eine Suche in Tracks auf.
pub async fn resolve(query: &str, source: SearchSource) -> Result<Vec<TrackMeta>> {
    let target = if is_url(query) {
        query.to_string()
    } else {
        match source {
            SearchSource::YouTube => format!("ytsearch1:{query}"),
            SearchSource::SoundCloud => format!("scsearch1:{query}"),
        }
    };

    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "--dump-single-json",
        "--flat-playlist",
        "--no-warnings",
        "--playlist-end",
        &MAX_PLAYLIST_TRACKS.to_string(),
        "--",
        &target,
    ])
    .stdin(Stdio::null())
    .kill_on_drop(true);

    let output = tokio::time::timeout(RESOLVE_TIMEOUT, cmd.output())
        .await
        .context("yt-dlp hat nicht rechtzeitig geantwortet")?
        .context("yt-dlp konnte nicht gestartet werden")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .unwrap_or("unbekannter Fehler");
        bail!("Konnte nicht geladen werden: {}", reason.trim());
    }

    let item: YtDlpItem =
        serde_json::from_slice(&output.stdout).context("Antwort von yt-dlp unlesbar")?;

    let tracks: Vec<TrackMeta> = if item.kind.as_deref() == Some("playlist") {
        item.entries
            .into_iter()
            .filter_map(YtDlpItem::into_meta)
            .take(MAX_PLAYLIST_TRACKS)
            .collect()
    } else {
        item.into_meta().into_iter().collect()
    };

    if tracks.is_empty() {
        bail!("Nichts gefunden");
    }
    Ok(tracks)
}
