//! Infos über den Discord-Server (Kanäle, Rollen) für die Weboberfläche.
//!
//! Die Weboberfläche kennt Discord nicht direkt; der Bot liefert diese Daten
//! über den Trait aus seinem Cache.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct NamedId {
    /// Als String, weil JavaScript 64-Bit-IDs nicht exakt darstellen kann.
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuildInfo {
    pub id: String,
    pub name: String,
    pub bot_name: String,
    pub voice_channels: Vec<NamedId>,
    pub text_channels: Vec<NamedId>,
    pub roles: Vec<NamedId>,
}

pub trait GuildDirectory: Send + Sync + 'static {
    /// `None`, solange der Bot nicht verbunden ist.
    fn guild_info(&self, guild_id: u64) -> Option<GuildInfo>;
    /// Menschliche Zuhörer im Kanal.
    fn listeners(&self, guild_id: u64, channel_id: u64) -> Option<usize>;
}
