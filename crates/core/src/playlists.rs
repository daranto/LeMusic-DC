//! Gespeicherte Playlists.
//!
//! Jede Änderung erhöht die `revision`. Bearbeitungen aus der Weboberfläche
//! geben die Revision mit, die sie kennen – hat sich die Playlist inzwischen
//! geändert (zweiter Tab, Discord-Befehl), wird nichts überschrieben.

use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{Db, TrackMeta};

pub const MAX_NAME_LEN: usize = 64;
pub const MAX_TRACKS: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistInfo {
    pub name: String,
    pub track_count: usize,
    pub duration_secs: u64,
    pub updated_at: String,
}

/// Eine Playlist mit allem, was die Weboberfläche zum Bearbeiten braucht.
#[derive(Debug, Clone)]
pub struct Playlist {
    pub name: String,
    pub tracks: Vec<TrackMeta>,
    pub revision: i64,
    /// Link der importierten YouTube-/SoundCloud-Playlist (für „Aktualisieren“).
    pub source: Option<String>,
    pub updated_at: String,
}

/// Die Playlist wurde zwischenzeitlich geändert.
#[derive(Debug)]
pub struct PlaylistConflict;

impl std::fmt::Display for PlaylistConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Die Playlist wurde inzwischen geändert – Ansicht wurde neu geladen")
    }
}

impl std::error::Error for PlaylistConflict {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    Title,
    Author,
    Duration,
    Shuffle,
    Reverse,
}

/// Eine Bearbeitung. Positionen beziehen sich auf den Stand vor genau
/// dieser Operation (mehrere Operationen werden nacheinander angewandt).
#[derive(Debug, Clone)]
pub enum PlaylistOp {
    /// Entfernt die Positionen.
    Remove(Vec<usize>),
    /// Verschiebt die Positionen (Reihenfolge bleibt) vor Position `to`
    /// (`to == len` = ans Ende).
    Move {
        indices: Vec<usize>,
        to: usize,
    },
    /// Fügt Titel an Position `index` ein.
    Insert {
        index: usize,
        tracks: Vec<TrackMeta>,
    },
    Sort(SortKey),
    /// Entfernt doppelte Titel (der erste bleibt).
    Dedupe,
    /// Ersetzt alles (für „Rückgängig“).
    Replace(Vec<TrackMeta>),
}

fn checked_indices(indices: &[usize], len: usize) -> Result<Vec<usize>> {
    let mut sorted: Vec<usize> = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        bail!("Keine Titel ausgewählt");
    }
    if sorted.last().is_some_and(|&i| i >= len) {
        bail!("Ungültige Position – bitte neu laden");
    }
    Ok(sorted)
}

fn sort_text(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Wendet Operationen auf eine Titelliste an. Liefert die Zahl der
/// betroffenen Titel.
pub fn apply_ops(tracks: &mut Vec<TrackMeta>, ops: Vec<PlaylistOp>) -> Result<usize> {
    let mut affected = 0;
    for op in ops {
        match op {
            PlaylistOp::Remove(indices) => {
                let set: HashSet<usize> = checked_indices(&indices, tracks.len())?
                    .into_iter()
                    .collect();
                let mut i = 0;
                tracks.retain(|_| {
                    let keep = !set.contains(&i);
                    i += 1;
                    keep
                });
                affected += set.len();
            }
            PlaylistOp::Move { indices, to } => {
                let sorted = checked_indices(&indices, tracks.len())?;
                if to > tracks.len() {
                    bail!("Ungültiges Ziel – bitte neu laden");
                }
                let set: HashSet<usize> = sorted.iter().copied().collect();
                let before = sorted.iter().filter(|&&i| i < to).count();
                let mut moved = Vec::with_capacity(sorted.len());
                let mut rest = Vec::with_capacity(tracks.len());
                for (i, t) in tracks.drain(..).enumerate() {
                    if set.contains(&i) {
                        moved.push(t);
                    } else {
                        rest.push(t);
                    }
                }
                let at = to - before;
                affected += moved.len();
                rest.splice(at..at, moved);
                *tracks = rest;
            }
            PlaylistOp::Insert { index, tracks: new } => {
                if index > tracks.len() {
                    bail!("Ungültige Position – bitte neu laden");
                }
                affected += new.len();
                tracks.splice(index..index, new);
            }
            PlaylistOp::Sort(key) => {
                match key {
                    SortKey::Title => tracks.sort_by_cached_key(|t| sort_text(&t.title)),
                    SortKey::Author => {
                        tracks.sort_by_cached_key(|t| (sort_text(&t.author), sort_text(&t.title)))
                    }
                    SortKey::Duration => tracks.sort_by_key(|t| t.duration),
                    SortKey::Shuffle => fastrand::shuffle(tracks),
                    SortKey::Reverse => tracks.reverse(),
                }
                affected += tracks.len();
            }
            PlaylistOp::Dedupe => {
                let mut seen = HashSet::new();
                let before = tracks.len();
                tracks.retain(|t| seen.insert(t.key()));
                affected += before - tracks.len();
            }
            PlaylistOp::Replace(new) => {
                affected += new.len();
                *tracks = new;
            }
        }
    }
    if tracks.len() > MAX_TRACKS {
        bail!("Eine Playlist darf höchstens {MAX_TRACKS} Tracks enthalten");
    }
    Ok(affected)
}

pub fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        bail!("Playlist-Name muss 1–{MAX_NAME_LEN} Zeichen lang sein");
    }
    Ok(name)
}

impl Db {
    pub async fn playlist_list(&self) -> Result<Vec<PlaylistInfo>> {
        let rows = sqlx::query(
            "SELECT name, json_array_length(tracks) AS n,
                    COALESCE((SELECT SUM(COALESCE(json_extract(value, '$.duration.secs'), 0))
                              FROM json_each(tracks)), 0) AS secs,
                    updated_at
             FROM playlists ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PlaylistInfo {
                    name: row.try_get("name")?,
                    track_count: usize::try_from(row.try_get::<i64, _>("n")?).unwrap_or(0),
                    duration_secs: u64::try_from(row.try_get::<i64, _>("secs")?).unwrap_or(0),
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    pub async fn playlist_get(&self, name: &str) -> Result<Option<Vec<TrackMeta>>> {
        Ok(self.playlist_load(name).await?.map(|p| p.tracks))
    }

    /// Lädt eine Playlist samt Revision und Quelle.
    pub async fn playlist_load(&self, name: &str) -> Result<Option<Playlist>> {
        let row = sqlx::query(
            "SELECT name, tracks, revision, source, updated_at FROM playlists WHERE name = ?",
        )
        .bind(name.trim())
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            let raw: String = row.try_get("tracks")?;
            Ok(Playlist {
                name: row.try_get("name")?,
                tracks: serde_json::from_str(&raw).context("Playlist ist beschädigt")?,
                revision: row.try_get("revision")?,
                source: row.try_get("source")?,
                updated_at: row.try_get("updated_at")?,
            })
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
                revision = revision + 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(name)
        .bind(serde_json::to_string(tracks)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Speichert nur, wenn die Playlist noch auf Revision `expected` steht.
    /// Liefert die neue Revision; sonst Fehler [`PlaylistConflict`].
    pub async fn playlist_update(
        &self,
        name: &str,
        expected: i64,
        tracks: &[TrackMeta],
    ) -> Result<i64> {
        if tracks.len() > MAX_TRACKS {
            bail!("Eine Playlist darf höchstens {MAX_TRACKS} Tracks enthalten");
        }
        let row = sqlx::query(
            "UPDATE playlists SET tracks = ?, revision = revision + 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE name = ? AND revision = ? RETURNING revision",
        )
        .bind(serde_json::to_string(tracks)?)
        .bind(name.trim())
        .bind(expected)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(row.try_get("revision")?),
            None if self.playlist_load(name).await?.is_some() => Err(PlaylistConflict.into()),
            None => bail!("Playlist nicht gefunden"),
        }
    }

    /// Hängt Tracks an eine Playlist an (legt sie bei Bedarf an).
    pub async fn playlist_append(&self, name: &str, tracks: &[TrackMeta]) -> Result<usize> {
        let mut all = self.playlist_get(name).await?.unwrap_or_default();
        all.extend_from_slice(tracks);
        self.playlist_save(name, &all).await?;
        Ok(all.len())
    }

    /// Benennt um; schlägt fehl, wenn es den neuen Namen schon gibt.
    pub async fn playlist_rename(&self, old: &str, new: &str) -> Result<()> {
        let new = validate_name(new)?;
        let old = old.trim();
        if !old.eq_ignore_ascii_case(new) && self.playlist_load(new).await?.is_some() {
            bail!("Eine Playlist „{new}“ gibt es schon");
        }
        let result = sqlx::query(
            "UPDATE playlists SET name = ?, revision = revision + 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE name = ?",
        )
        .bind(new)
        .bind(old)
        .execute(self.pool())
        .await?;
        if result.rows_affected() == 0 {
            bail!("Playlist nicht gefunden");
        }
        Ok(())
    }

    /// Legt eine neue Playlist an; schlägt fehl, wenn es sie schon gibt.
    pub async fn playlist_create(
        &self,
        name: &str,
        tracks: &[TrackMeta],
        source: Option<&str>,
    ) -> Result<()> {
        let name = validate_name(name)?;
        if tracks.len() > MAX_TRACKS {
            bail!("Eine Playlist darf höchstens {MAX_TRACKS} Tracks enthalten");
        }
        if self.playlist_load(name).await?.is_some() {
            bail!("Eine Playlist „{name}“ gibt es schon");
        }
        sqlx::query("INSERT INTO playlists (name, tracks, source) VALUES (?, ?, ?)")
            .bind(name)
            .bind(serde_json::to_string(tracks)?)
            .bind(source)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Merkt sich (oder vergisst) den Quell-Link einer Playlist.
    pub async fn playlist_set_source(&self, name: &str, source: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE playlists SET source = ? WHERE name = ?")
            .bind(source)
            .bind(name.trim())
            .execute(self.pool())
            .await?;
        Ok(())
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

    fn titles(tracks: &[TrackMeta]) -> Vec<String> {
        tracks.iter().map(|t| t.title.clone()).collect()
    }

    fn five() -> Vec<TrackMeta> {
        (1..=5).map(track).collect()
    }

    #[test]
    fn move_keeps_order_of_selection() -> Result<()> {
        // 2 und 4 vor 1 → 2 4 1 3 5
        let mut t = five();
        apply_ops(
            &mut t,
            vec![PlaylistOp::Move {
                indices: vec![3, 1],
                to: 0,
            }],
        )?;
        assert_eq!(
            titles(&t),
            ["Track 2", "Track 4", "Track 1", "Track 3", "Track 5"]
        );
        // 1 ans Ende
        let mut t = five();
        apply_ops(
            &mut t,
            vec![PlaylistOp::Move {
                indices: vec![0],
                to: 5,
            }],
        )?;
        assert_eq!(
            titles(&t),
            ["Track 2", "Track 3", "Track 4", "Track 5", "Track 1"]
        );
        // 1 vor 4 (nach unten ziehen)
        let mut t = five();
        apply_ops(
            &mut t,
            vec![PlaylistOp::Move {
                indices: vec![0],
                to: 3,
            }],
        )?;
        assert_eq!(
            titles(&t),
            ["Track 2", "Track 3", "Track 1", "Track 4", "Track 5"]
        );
        assert!(
            apply_ops(
                &mut t,
                vec![PlaylistOp::Move {
                    indices: vec![9],
                    to: 0
                }]
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn remove_insert_dedupe_sort() -> Result<()> {
        let mut t = five();
        let removed = apply_ops(&mut t, vec![PlaylistOp::Remove(vec![4, 0, 0])])?;
        assert_eq!(removed, 2);
        assert_eq!(titles(&t), ["Track 2", "Track 3", "Track 4"]);
        apply_ops(
            &mut t,
            vec![PlaylistOp::Insert {
                index: 1,
                tracks: vec![track(2), track(9)],
            }],
        )?;
        assert_eq!(apply_ops(&mut t, vec![PlaylistOp::Dedupe])?, 1);
        assert_eq!(titles(&t), ["Track 2", "Track 9", "Track 3", "Track 4"]);
        apply_ops(&mut t, vec![PlaylistOp::Sort(SortKey::Reverse)])?;
        assert_eq!(titles(&t)[0], "Track 4");
        apply_ops(&mut t, vec![PlaylistOp::Sort(SortKey::Title)])?;
        assert_eq!(titles(&t)[3], "Track 9");
        Ok(())
    }

    #[tokio::test]
    async fn revision_protects_against_lost_updates() -> Result<()> {
        let db = Db::in_memory().await?;
        db.playlist_save("Mix", &[track(1)]).await?;
        let rev = db
            .playlist_load("mix")
            .await?
            .map(|p| p.revision)
            .unwrap_or_default();
        let next = db.playlist_update("mix", rev, &[track(2)]).await?;
        assert_eq!(next, rev + 1);
        // Veralteter Stand → Konflikt, nichts überschrieben.
        let err = db.playlist_update("mix", rev, &[track(3)]).await;
        assert!(err.is_err_and(|e| e.downcast_ref::<PlaylistConflict>().is_some()));
        assert_eq!(
            db.playlist_get("mix").await?.unwrap_or_default(),
            vec![track(2)]
        );
        // Anhängen (Discord) erhöht die Revision ebenfalls.
        db.playlist_append("mix", &[track(4)]).await?;
        assert!(db.playlist_update("mix", next, &[]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn rename_and_create() -> Result<()> {
        let db = Db::in_memory().await?;
        db.playlist_create(
            "A",
            &[track(1)],
            Some("https://youtube.com/playlist?list=x"),
        )
        .await?;
        db.playlist_save("B", &[]).await?;
        assert!(db.playlist_create("a", &[], None).await.is_err());
        assert!(db.playlist_rename("A", "b").await.is_err());
        db.playlist_rename("A", "Neu").await?;
        db.playlist_rename("neu", "NEU").await?;
        let p = db
            .playlist_load("neu")
            .await?
            .ok_or_else(|| anyhow::anyhow!("fehlt"))?;
        assert_eq!(p.name, "NEU");
        assert_eq!(
            p.source.as_deref(),
            Some("https://youtube.com/playlist?list=x")
        );
        assert!(db.playlist_load("A").await?.is_none());
        let list = db.playlist_list().await?;
        assert_eq!(
            list.iter()
                .find(|l| l.name == "NEU")
                .map(|l| l.duration_secs),
            Some(60)
        );
        Ok(())
    }
}
