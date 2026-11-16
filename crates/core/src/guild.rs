//! Einstellungen pro Discord-Server.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::Db;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuildSettings {
    /// Rolle mit DJ-Rechten (Skip, Stop, Lautstärke …). `None` = alle dürfen.
    pub dj_role: Option<u64>,
    /// Befehle nur in diesem Textkanal erlauben. `None` = überall.
    pub text_channel: Option<u64>,
    /// Bot darf nur diesen Sprachkanal nutzen. `None` = beliebig.
    pub voice_channel: Option<u64>,
    /// Anteil der Zuhörer, der für einen Vote-Skip nötig ist (0.0–1.0).
    pub skip_ratio: f32,
    /// Maximale Track-Länge in Sekunden, 0 = unbegrenzt.
    pub max_track_secs: u64,
    /// Name der Playlist, die läuft, wenn die Warteschlange leer ist.
    pub autoplaylist: Option<String>,
    /// „Spiele jetzt“-Nachrichten im Textkanal senden.
    pub announce_tracks: bool,
    /// Fester Kanal für alle Ansagen. `None` = Kanal des letzten Befehls
    /// bzw. der erlaubte Befehlskanal.
    pub announce_channel: Option<u64>,
    /// Automatisch beitreten, sobald jemand einen Sprachkanal betritt.
    pub auto_join: bool,
    /// Sprachkanäle für das automatische Beitreten. Leer = alle.
    pub auto_join_channels: Vec<u64>,
}

impl Default for GuildSettings {
    fn default() -> Self {
        Self {
            dj_role: None,
            text_channel: None,
            voice_channel: None,
            skip_ratio: 0.55,
            max_track_secs: 0,
            autoplaylist: None,
            announce_tracks: true,
            announce_channel: None,
            auto_join: false,
            auto_join_channels: Vec::new(),
        }
    }
}

impl GuildSettings {
    fn key(guild_id: u64) -> String {
        format!("guild:{guild_id}")
    }

    /// Wohin Ansagen gehen: fester Ansage-Kanal, sonst der Kanal des letzten
    /// Befehls, sonst der erlaubte Befehlskanal.
    pub fn announce_target(&self, last_command_channel: Option<u64>) -> Option<u64> {
        self.announce_channel
            .or(last_command_channel)
            .or(self.text_channel)
    }

    /// Darf der Bot diesem Kanal automatisch beitreten?
    pub fn auto_join_allowed(&self, channel: u64) -> bool {
        self.auto_join
            && self.voice_channel.is_none_or(|only| only == channel)
            && (self.auto_join_channels.is_empty() || self.auto_join_channels.contains(&channel))
    }

    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.skip_ratio) || self.skip_ratio.is_nan() {
            bail!("Skip-Quote muss zwischen 0 und 100 % liegen");
        }
        Ok(())
    }

    pub async fn load(db: &Db, guild_id: u64) -> Result<Self> {
        Ok(db.get_json(&Self::key(guild_id)).await?.unwrap_or_default())
    }

    pub async fn save(&self, db: &Db, guild_id: u64) -> Result<()> {
        self.validate()?;
        db.set_json(&Self::key(guild_id), self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_join_rules() {
        let mut s = GuildSettings::default();
        assert!(!s.auto_join_allowed(1));
        s.auto_join = true;
        assert!(s.auto_join_allowed(1) && s.auto_join_allowed(2));
        s.auto_join_channels = vec![2, 3];
        assert!(!s.auto_join_allowed(1) && s.auto_join_allowed(2));
        // „Nur in Sprachkanal“ hat Vorrang.
        s.voice_channel = Some(3);
        assert!(!s.auto_join_allowed(2) && s.auto_join_allowed(3));
        // Ältere gespeicherte Einstellungen ohne die Felder bleiben lesbar.
        let old: GuildSettings = serde_json::from_str(r#"{"skip_ratio":0.5}"#).unwrap_or_default();
        assert!(!old.auto_join && old.auto_join_channels.is_empty());
    }

    #[test]
    fn announce_target_priority() {
        let mut s = GuildSettings::default();
        assert_eq!(s.announce_target(None), None);
        assert_eq!(s.announce_target(Some(2)), Some(2));
        s.text_channel = Some(3);
        assert_eq!(s.announce_target(None), Some(3));
        assert_eq!(s.announce_target(Some(2)), Some(2));
        s.announce_channel = Some(1);
        assert_eq!(s.announce_target(Some(2)), Some(1));
    }
}
