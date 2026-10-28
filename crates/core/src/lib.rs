//! Gemeinsame Grundlagen: Konfiguration, Datenbank, Einstellungen, Health.

pub mod config;
pub mod db;
pub mod guild;
pub mod health;
pub mod playlists;
pub mod settings;
pub mod track;

pub use config::{EnvConfig, LogFormat};
pub use db::Db;
pub use guild::GuildSettings;
pub use health::Health;
pub use settings::{BotSettings, SettingsStore};
pub use track::{TrackMeta, format_duration, parse_duration};
