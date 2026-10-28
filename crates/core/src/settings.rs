//! Zur Laufzeit änderbare Einstellungen.
//!
//! Gespeichert in SQLite, verteilt über einen `watch`-Channel: Wer
//! [`SettingsStore::subscribe`] nutzt, bekommt Änderungen sofort mit – ohne
//! Neustart. Später schreibt die Weboberfläche hierüber.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Db;

const KEY: &str = "bot";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BotSettings {
    /// Text im Discord-Status („Hört …“). Leer = kein Status.
    pub status_text: String,
    /// Lautstärke neuer Tracks in Prozent (0–200).
    pub default_volume: u16,
    /// Nach so vielen Sekunden ohne Wiedergabe verlässt der Bot den Kanal.
    pub idle_timeout_secs: u64,
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            status_text: "/play".into(),
            default_volume: 100,
            idle_timeout_secs: 300,
        }
    }
}

impl BotSettings {
    pub const MAX_VOLUME: u16 = 200;
    pub const MAX_STATUS_LEN: usize = 128;

    pub fn validate(&self) -> Result<()> {
        if self.default_volume > Self::MAX_VOLUME {
            bail!("Lautstärke muss zwischen 0 und {} liegen", Self::MAX_VOLUME);
        }
        if self.status_text.chars().count() > Self::MAX_STATUS_LEN {
            bail!(
                "Status-Text darf höchstens {} Zeichen haben",
                Self::MAX_STATUS_LEN
            );
        }
        Ok(())
    }

    /// Lautstärke als Faktor für songbird (1.0 = 100 %).
    pub fn volume_factor(&self) -> f32 {
        f32::from(self.default_volume) / 100.0
    }
}

#[derive(Clone)]
pub struct SettingsStore {
    db: Db,
    tx: Arc<watch::Sender<BotSettings>>,
}

impl SettingsStore {
    /// Lädt die Einstellungen; fehlen sie, werden die Standardwerte gespeichert.
    pub async fn load(db: Db) -> Result<Self> {
        let settings = match db.get_json::<BotSettings>(KEY).await? {
            Some(s) => s,
            None => {
                let s = BotSettings::default();
                db.set_json(KEY, &s).await?;
                s
            }
        };
        let (tx, _) = watch::channel(settings);
        Ok(Self {
            db,
            tx: Arc::new(tx),
        })
    }

    pub fn current(&self) -> BotSettings {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<BotSettings> {
        self.tx.subscribe()
    }

    /// Prüft, speichert und verteilt neue Einstellungen.
    pub async fn update(&self, new: BotSettings) -> Result<()> {
        new.validate()?;
        self.db.set_json(KEY, &new).await?;
        self.tx.send_replace(new);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn defaults_are_stored_and_updates_propagate() -> Result<()> {
        let db = Db::in_memory().await?;
        let store = SettingsStore::load(db.clone()).await?;
        assert_eq!(store.current(), BotSettings::default());

        let mut rx = store.subscribe();
        let changed = BotSettings {
            default_volume: 50,
            ..BotSettings::default()
        };
        store.update(changed.clone()).await?;
        assert!(rx.has_changed()?);
        assert_eq!(*rx.borrow_and_update(), changed);

        // Neu laden liest den gespeicherten Wert.
        let reloaded = SettingsStore::load(db).await?;
        assert_eq!(reloaded.current(), changed);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_settings_are_rejected() -> Result<()> {
        let store = SettingsStore::load(Db::in_memory().await?).await?;
        let invalid = BotSettings {
            default_volume: 500,
            ..BotSettings::default()
        };
        assert!(store.update(invalid).await.is_err());
        assert_eq!(store.current(), BotSettings::default());
        Ok(())
    }

    #[test]
    fn missing_fields_get_defaults() -> Result<()> {
        let parsed: BotSettings = serde_json::from_str(r#"{"default_volume": 80}"#)?;
        assert_eq!(parsed.default_volume, 80);
        assert_eq!(
            parsed.idle_timeout_secs,
            BotSettings::default().idle_timeout_secs
        );
        Ok(())
    }
}
