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
