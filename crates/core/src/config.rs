//! Startkonfiguration aus Umgebungsvariablen.
//!
//! Hier steht nur, was vor dem Start feststehen muss (Token, Pfade, Ports).
//! Alles, was sich zur Laufzeit ändern lässt, liegt in [`crate::settings`].

use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context as _, Result, bail};

pub const DEFAULT_WEB_BIND: &str = "0.0.0.0:8080";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    /// Liest `LOG_FORMAT` (`text` oder `json`), Standard ist `text`.
    pub fn from_env() -> Self {
        match env::var("LOG_FORMAT").as_deref() {
            Ok("json") => Self::Json,
            _ => Self::Text,
        }
    }
}

#[derive(Clone)]
pub struct EnvConfig {
    pub discord_token: String,
    pub guild_id: u64,
    pub data_dir: PathBuf,
    pub web_bind: SocketAddr,
}

// Token nie ins Log schreiben.
impl std::fmt::Debug for EnvConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvConfig")
            .field("discord_token", &"<verborgen>")
            .field("guild_id", &self.guild_id)
            .field("data_dir", &self.data_dir)
            .field("web_bind", &self.web_bind)
            .finish()
    }
}

impl EnvConfig {
    pub fn from_env() -> Result<Self> {
        let required = |name: &str| -> Result<String> {
            let value =
                env::var(name).with_context(|| format!("Umgebungsvariable {name} fehlt"))?;
            let value = value.trim().to_string();
            if value.is_empty() {
                bail!("Umgebungsvariable {name} ist leer");
            }
            Ok(value)
        };

        Ok(Self {
            discord_token: required("DISCORD_TOKEN")?,
            guild_id: required("GUILD_ID")?
                .parse()
                .context("GUILD_ID ist keine gültige Zahl")?,
            data_dir: env::var("DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/data")),
            web_bind: web_bind_from_env()?,
        })
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("lemusic.db")
    }
}

/// `WEB_BIND`, z. B. `0.0.0.0:8080`. Auch vom Healthcheck genutzt.
pub fn web_bind_from_env() -> Result<SocketAddr> {
    env::var("WEB_BIND")
        .unwrap_or_else(|_| DEFAULT_WEB_BIND.to_string())
        .parse()
        .context("WEB_BIND ist keine gültige Adresse (z. B. 0.0.0.0:8080)")
}
