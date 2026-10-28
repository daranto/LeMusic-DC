//! Gespeicherte Playlists.

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use sqlx::Row;

use crate::{Db, TrackMeta};

pub const MAX_NAME_LEN: usize = 64;
pub const MAX_TRACKS: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistInfo {
    pub name: String,
    pub track_count: usize,
    pub updated_at: String,
}

fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        bail!("Playlist-Name muss 1–{MAX_NAME_LEN} Zeichen lang sein");
    }
    Ok(name)
}

impl Db {
    pub async fn playlist_list(&self) -> Result<Vec<PlaylistInfo>> {
        let rows = sqlx::query(
            "SELECT name, json_array_length(tracks) AS n, updated_at FROM playlists ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PlaylistInfo {
                    name: row.try_get("name")?,
                    track_count: usize::try_from(row.try_get::<i64, _>("n")?).unwrap_or(0),
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    pub async fn playlist_get(&self, name: &str) -> Result<Option<Vec<TrackMeta>>> {
        let row = sqlx::query("SELECT tracks FROM playlists WHERE name = ?")
            .bind(name.trim())
            .fetch_optional(self.pool())
            .await?;
        row.map(|row| {
            let raw: String = row.try_get("tracks")?;
            serde_json::from_str(&raw).context("Playlist ist beschädigt")
        })
        .transpose()
    }

    /// Speichert (oder ersetzt) eine Playlist.
    pub async fn playlist_save(&self, name: &str, tracks: &[TrackMeta]) -> Result<()> {
        let name = validate_name(name)?;
        if tracks.len() > MAX_TRACKS {
            bail!("Eine Playlist darf höchstens {MAX_TRACKS} Tracks enthalten");
        }
        sqlx::query(
            "INSERT INTO playlists (name, tracks) VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET
                tracks = excluded.tracks,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(name)
        .bind(serde_json::to_string(tracks)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Hängt Tracks an eine Playlist an (legt sie bei Bedarf an).
    pub async fn playlist_append(&self, name: &str, tracks: &[TrackMeta]) -> Result<usize> {
        let mut all = self.playlist_get(name).await?.unwrap_or_default();
        all.extend_from_slice(tracks);
        self.playlist_save(name, &all).await?;
        Ok(all.len())
    }

    /// Löscht eine Playlist; `false`, wenn es sie nicht gab.
    pub async fn playlist_delete(&self, name: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM playlists WHERE name = ?")
            .bind(name.trim())
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn track(n: u32) -> TrackMeta {
        TrackMeta {
            title: format!("Track {n}"),
            author: "Test".into(),
            url: format!("https://example.com/{n}"),
            duration: Some(Duration::from_secs(60)),
            thumbnail: None,
        }
    }

    #[tokio::test]
    async fn playlist_lifecycle() -> Result<()> {
        let db = Db::in_memory().await?;
        db.playlist_save("Party", &[track(1), track(2)]).await?;
        assert_eq!(db.playlist_append("party", &[track(3)]).await?, 3);

        let list = db.playlist_list().await?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].track_count, 3);

        let tracks = db.playlist_get("PARTY").await?.unwrap_or_default();
        assert_eq!(tracks[2], track(3));

        assert!(db.playlist_delete("party").await?);
        assert!(!db.playlist_delete("party").await?);
        assert!(db.playlist_get("party").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn invalid_names_are_rejected() -> Result<()> {
        let db = Db::in_memory().await?;
        assert!(db.playlist_save("   ", &[]).await.is_err());
        assert!(db.playlist_save(&"x".repeat(65), &[]).await.is_err());
        Ok(())
    }
}
