//! SQLite-Datenbank mit automatischen Migrationen.

use std::{path::Path, str::FromStr, time::Duration};

use anyhow::{Context as _, Result};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{
    Row,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
    },
};

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Öffnet (oder erstellt) die Datenbank und spielt Migrationen ein.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Datenverzeichnis {} nicht anlegbar", dir.display()))?;
        }

        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            // WAL: Lesen blockiert Schreiben nicht, robust bei Abstürzen.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .with_context(|| format!("Datenbank {} nicht öffnbar", path.display()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("Datenbank-Migration fehlgeschlagen")?;

        Ok(Self { pool })
    }

    /// Öffnet eine leere In-Memory-Datenbank (für Tests).
    pub async fn in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Prüft, ob die Datenbank antwortet.
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let raw: String = row.try_get("value")?;
            serde_json::from_str(&raw)
                .with_context(|| format!("Einstellung „{key}“ ist beschädigt"))
        })
        .transpose()
    }

    pub async fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(key)
        .bind(raw)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Schließt alle Verbindungen (beim Herunterfahren).
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
