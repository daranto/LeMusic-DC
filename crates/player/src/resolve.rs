//! Auflösen von Links und Suchanfragen über yt-dlp.
//!
//! Hier werden nur Metadaten geholt (`--flat-playlist`). Die eigentliche
//! Stream-URL wird erst beim Abspielen ermittelt, weil sie nach einigen
//! Stunden abläuft.

use std::{process::Stdio, time::Duration};

use anyhow::{Context as _, Result, bail};
use lemusic_core::TrackMeta;
use serde::Deserialize;
use tokio::process::Command;

/// Maximale Laufzeit eines yt-dlp-Aufrufs.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(60);
/// Obergrenze für Playlists (1000 Einträge lädt yt-dlp in ~8 s).
pub const MAX_PLAYLIST_TRACKS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    YouTube,
    SoundCloud,
}

impl SearchSource {
    fn prefix(self) -> &'static str {
        match self {
            Self::YouTube => "ytsearch",
            Self::SoundCloud => "scsearch",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Thumbnail {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YtDlpItem {
    #[serde(rename = "_type")]
    kind: Option<String>,
    id: Option<String>,
    ie_key: Option<String>,
    extractor_key: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    duration: Option<f64>,
    thumbnail: Option<String>,
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
    #[serde(default)]
    entries: Vec<YtDlpItem>,
}

impl YtDlpItem {
    fn is_youtube(&self) -> bool {
        let key = self.ie_key.as_deref().or(self.extractor_key.as_deref());
        key.is_some_and(|k| k.starts_with("Youtube"))
    }

    fn into_meta(self) -> Option<TrackMeta> {
        let youtube_thumb = self
            .is_youtube()
            .then_some(self.id.as_ref())
            .flatten()
            .map(|id| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));
        let thumbnail = self
            .thumbnail
            .or_else(|| self.thumbnails.into_iter().rev().find_map(|t| t.url))
            .or(youtube_thumb);
        let url = self.webpage_url.or(self.url)?;
        if !is_url(&url) {
            return None;
        }
        Some(TrackMeta {
            title: self.title.unwrap_or_else(|| url.clone()),
            author: self.uploader.or(self.channel).unwrap_or_default(),
            url,
            duration: self
                .duration
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(Duration::from_secs_f64),
            thumbnail,
        })
    }
}

/// Fehler, bei denen ein erneuter Versuch sinnlos ist – mit verständlicher
/// Begründung für die Nachricht im Kanal.
pub fn permanent_reason(error: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        (
            "confirm your age",
            "altersbeschränkt (dafür sind YouTube-Cookies nötig)",
        ),
        ("Private video", "privates Video"),
        ("members-only", "nur für Kanalmitglieder"),
        ("Join this channel", "nur für Kanalmitglieder"),
        (
            "not available in your country",
            "in diesem Land nicht verfügbar",
        ),
        ("copyright", "wegen Urheberrecht gesperrt"),
        ("has been removed", "wurde entfernt"),
        ("Video unavailable", "nicht verfügbar"),
        ("This video is not available", "nicht verfügbar"),
        ("HTTP Error 404", "nicht gefunden"),
    ];
    KNOWN
        .iter()
        .find(|(needle, _)| error.contains(needle))
        .map(|(_, reason)| *reason)
}

pub fn is_url(query: &str) -> bool {
    query.starts_with("https://") || query.starts_with("http://")
}

/// Löst einen Link (Track oder Playlist) oder eine Suche (erster Treffer) auf.
pub async fn resolve(query: &str, source: SearchSource) -> Result<Vec<TrackMeta>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("Bitte einen Link oder Suchbegriff angeben");
    }
    if is_url(query) {
        run_ytdlp(query).await
    } else {
        search(query, source, 1).await
    }
}

/// Sucht und liefert bis zu `limit` Treffer.
pub async fn search(query: &str, source: SearchSource, limit: usize) -> Result<Vec<TrackMeta>> {
    let limit = limit.clamp(1, 10);
    run_ytdlp(&format!("{}{limit}:{}", source.prefix(), query.trim())).await
}

async fn run_ytdlp(target: &str) -> Result<Vec<TrackMeta>> {
    let mut cmd = Command::new("yt-dlp");
    cmd.args(crate::ytdlp_base_args())
        .args([
            "--dump-single-json",
            "--flat-playlist",
            "--playlist-end",
            &MAX_PLAYLIST_TRACKS.to_string(),
            "--",
            target,
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
            .map(|l| l.trim_start_matches("ERROR:").trim())
            .unwrap_or("unbekannter Fehler");
        if let Some(reason) = permanent_reason(reason) {
            bail!("Nicht abspielbar: {reason}");
        }
        // yt-dlp hängt gern lange Hilfetexte an – erster Satz reicht.
        let short = reason.split(". ").next().unwrap_or(reason);
        bail!("Konnte nicht geladen werden: {short}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_youtube_entry_gets_thumbnail() -> Result<()> {
        let item: YtDlpItem = serde_json::from_str(
            r#"{"id":"abc","ie_key":"Youtube","title":"T","url":"https://www.youtube.com/watch?v=abc","duration":61.0}"#,
        )?;
        let meta = item
            .into_meta()
            .ok_or_else(|| anyhow::anyhow!("kein Track"))?;
        assert_eq!(
            meta.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/abc/hqdefault.jpg")
        );
        assert_eq!(meta.duration, Some(Duration::from_secs(61)));
        Ok(())
    }

    #[test]
    fn entries_without_url_are_skipped() -> Result<()> {
        let item: YtDlpItem = serde_json::from_str(r#"{"title":"kaputt","url":"abc"}"#)?;
        assert!(item.into_meta().is_none());
        Ok(())
    }
}
