//! Admin-Account und Login-Sitzungen der Weboberfläche.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{
        SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::Db;

const ADMIN_KEY: &str = "admin";
pub const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminAccount {
    username: String,
    password_hash: String,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Argon2 ist absichtlich langsam – nicht auf dem Async-Runtime-Thread.
async fn hash_password(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| anyhow!("Passwort-Hash fehlgeschlagen: {e}"))
    })
    .await?
}

async fn verify_password(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ergebnis der Admin-Einrichtung beim Start.
pub enum AdminBootstrap {
    /// Account existierte schon.
    Existing,
    /// Aus `ADMIN_USER`/`ADMIN_PASSWORD` angelegt.
    CreatedFromEnv,
    /// Kein Passwort vorgegeben – zufälliges erzeugt (einmalig anzeigen!).
    CreatedRandom { username: String, password: String },
}

impl Db {
    /// Legt beim ersten Start den Admin-Account an.
    pub async fn ensure_admin(
        &self,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<AdminBootstrap> {
        if self.get_json::<AdminAccount>(ADMIN_KEY).await?.is_some() {
            return Ok(AdminBootstrap::Existing);
        }
        let username = username
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "admin".into());
        let (password, generated) = match password.filter(|p| !p.is_empty()) {
            Some(p) => {
                if p.chars().count() < MIN_PASSWORD_LEN {
                    bail!("ADMIN_PASSWORD muss mindestens {MIN_PASSWORD_LEN} Zeichen haben");
                }
                (p, false)
            }
            None => (random_token(9), true),
        };
        let account = AdminAccount {
            username: username.clone(),
            password_hash: hash_password(password.clone()).await?,
        };
        self.set_json(ADMIN_KEY, &account).await?;
        Ok(if generated {
            AdminBootstrap::CreatedRandom { username, password }
        } else {
            AdminBootstrap::CreatedFromEnv
        })
    }

    /// Prüft Login-Daten; bei Erfolg wird eine Sitzung angelegt (Token).
    pub async fn login(&self, username: &str, password: &str) -> Result<Option<String>> {
        let account: AdminAccount = self
            .get_json(ADMIN_KEY)
            .await?
            .context("Kein Admin-Account vorhanden")?;
        // Auch bei falschem Namen hashen, damit die Antwortzeit nichts verrät.
        let name_ok = account.username == username.trim();
        let pass_ok = verify_password(password.to_string(), account.password_hash).await;
        if !(name_ok && pass_ok) {
            return Ok(None);
        }
        let token = random_token(32);
        let expires = now_secs() + i64::try_from(SESSION_TTL.as_secs()).unwrap_or(i64::MAX);
        sqlx::query("INSERT INTO sessions (token_hash, username, expires_at) VALUES (?, ?, ?)")
            .bind(token_hash(&token))
            .bind(&account.username)
            .bind(expires)
            .execute(self.pool())
            .await?;
        Ok(Some(token))
    }

    /// Benutzername zur Sitzung, falls gültig.
    pub async fn session_user(&self, token: &str) -> Result<Option<String>> {
        let row =
            sqlx::query("SELECT username FROM sessions WHERE token_hash = ? AND expires_at > ?")
                .bind(token_hash(token))
                .bind(now_secs())
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map(|r| r.try_get("username")).transpose()?)
    }

    pub async fn logout(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash(token))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn purge_expired_sessions(&self) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now_secs())
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected())
    }

    /// Ändert das Passwort und beendet alle anderen Sitzungen.
    pub async fn change_password(&self, current: &str, new: &str, keep_token: &str) -> Result<()> {
        let mut account: AdminAccount = self
            .get_json(ADMIN_KEY)
            .await?
            .context("Kein Admin-Account vorhanden")?;
        if !verify_password(current.to_string(), account.password_hash.clone()).await {
            bail!("Aktuelles Passwort ist falsch");
        }
        if new.chars().count() < MIN_PASSWORD_LEN {
            bail!("Neues Passwort muss mindestens {MIN_PASSWORD_LEN} Zeichen haben");
        }
        account.password_hash = hash_password(new.to_string()).await?;
        self.set_json(ADMIN_KEY, &account).await?;
        sqlx::query("DELETE FROM sessions WHERE token_hash != ?")
            .bind(token_hash(keep_token))
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_flow() -> Result<()> {
        let db = Db::in_memory().await?;
        assert!(matches!(
            db.ensure_admin(Some("robin".into()), Some("geheim123".into()))
                .await?,
            AdminBootstrap::CreatedFromEnv
        ));
        assert!(matches!(
            db.ensure_admin(None, None).await?,
            AdminBootstrap::Existing
        ));

        assert!(db.login("robin", "falsch").await?.is_none());
        assert!(db.login("jemand", "geheim123").await?.is_none());
        let token = db
            .login("robin", "geheim123")
            .await?
            .ok_or_else(|| anyhow!("Login fehlgeschlagen"))?;
        assert_eq!(db.session_user(&token).await?.as_deref(), Some("robin"));
        assert!(db.session_user("unsinn").await?.is_none());

        let other = db
            .login("robin", "geheim123")
            .await?
            .ok_or_else(|| anyhow!("Login fehlgeschlagen"))?;
        assert!(
            db.change_password("falsch", "neuespasswort", &token)
                .await
                .is_err()
        );
        db.change_password("geheim123", "neuespasswort", &token)
            .await?;
        // Andere Sitzungen sind beendet, die eigene bleibt.
        assert!(db.session_user(&other).await?.is_none());
        assert!(db.session_user(&token).await?.is_some());
        assert!(db.login("robin", "neuespasswort").await?.is_some());

        db.logout(&token).await?;
        assert!(db.session_user(&token).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn random_password_is_generated() -> Result<()> {
        let db = Db::in_memory().await?;
        let AdminBootstrap::CreatedRandom { username, password } =
            db.ensure_admin(None, None).await?
        else {
            bail!("erwartet: zufälliges Passwort");
        };
        assert_eq!(username, "admin");
        assert!(password.len() >= MIN_PASSWORD_LEN);
        assert!(db.login(&username, &password).await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn short_env_password_is_rejected() -> Result<()> {
        let db = Db::in_memory().await?;
        assert!(db.ensure_admin(None, Some("kurz".into())).await.is_err());
        Ok(())
    }
}
