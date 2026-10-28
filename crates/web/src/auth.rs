//! Login, Sitzungs-Cookie, CSRF-Schutz und Bremse gegen Passwort-Raten.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use lemusic_core::auth::SESSION_TTL;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use crate::{WebState, api::ApiError};

const COOKIE: &str = "lemusic_session";
/// Header, den nur unsere eigene Seite setzt (Schutz vor CSRF).
const CSRF_HEADER: &str = "x-lemusic";
const MAX_FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const LOCKOUT: Duration = Duration::from_secs(60);

/// Nach zu vielen Fehlversuchen wird der Login kurz gesperrt.
#[derive(Default)]
pub(crate) struct LoginLimiter {
    failures: Mutex<Vec<Instant>>,
}

impl LoginLimiter {
    fn locked_for(&self) -> Option<Duration> {
        let mut failures = self.failures.lock().ok()?;
        failures.retain(|t| t.elapsed() < FAILURE_WINDOW);
        if failures.len() < MAX_FAILURES {
            return None;
        }
        let last = failures.last()?;
        LOCKOUT.checked_sub(last.elapsed())
    }

    fn record_failure(&self) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.push(Instant::now());
        }
    }

    fn reset(&self) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.clear();
        }
    }
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

fn cookie_header(token: &str, max_age: u64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn has_csrf_header(parts: &Parts) -> bool {
    parts.headers.get(CSRF_HEADER).is_some()
}

/// Extraktor: Anfrage stammt vom angemeldeten Admin.
pub(crate) struct Admin {
    pub username: String,
    pub token: String,
}

impl FromRequestParts<WebState> for Admin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &WebState,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Bitte anmelden" })),
            )
                .into_response()
        };
        if parts.method != Method::GET && !has_csrf_header(parts) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Ungültige Anfrage" })),
            )
                .into_response());
        }
        let Some(token) = session_token(&parts.headers) else {
            return Err(unauthorized());
        };
        match state.db.session_user(&token).await {
            Ok(Some(username)) => Ok(Admin { username, token }),
            Ok(None) => Err(unauthorized()),
            Err(e) => Err(ApiError::from(e).into_response()),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct LoginForm {
    username: String,
    password: String,
}

pub(crate) async fn login(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(form): Json<LoginForm>,
) -> Result<Response, ApiError> {
    if headers.get(CSRF_HEADER).is_none() {
        return Err(ApiError::forbidden("Ungültige Anfrage"));
    }
    if let Some(wait) = state.limiter.locked_for() {
        return Err(ApiError::too_many(&format!(
            "Zu viele Fehlversuche – bitte {} s warten",
            wait.as_secs().max(1)
        )));
    }
    match state.db.login(&form.username, &form.password).await? {
        Some(token) => {
            state.limiter.reset();
            info!(user = %form.username.trim(), "Anmeldung an der Weboberfläche");
            let mut response = Json(json!({ "username": form.username.trim() })).into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                cookie_header(&token, SESSION_TTL.as_secs()),
            );
            Ok(response)
        }
        None => {
            state.limiter.record_failure();
            warn!("Fehlgeschlagene Anmeldung an der Weboberfläche");
            // Gleichmäßige Antwortzeit, macht Raten zusätzlich mühsam.
            tokio::time::sleep(Duration::from_millis(500)).await;
            Err(ApiError::unauthorized("Benutzername oder Passwort falsch"))
        }
    }
}

pub(crate) async fn logout(
    State(state): State<WebState>,
    admin: Admin,
) -> Result<Response, ApiError> {
    state.db.logout(&admin.token).await?;
    let mut response = Json(json!({ "ok": true })).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header("", 0));
    Ok(response)
}

pub(crate) async fn me(admin: Admin) -> Json<serde_json::Value> {
    Json(json!({ "username": admin.username }))
}

#[derive(Deserialize)]
pub(crate) struct PasswordForm {
    current: String,
    new: String,
}

pub(crate) async fn change_password(
    State(state): State<WebState>,
    admin: Admin,
    Json(form): Json<PasswordForm>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .db
        .change_password(&form.current, &form.new, &admin.token)
        .await
        .map_err(ApiError::bad_request)?;
    info!("Admin-Passwort geändert");
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("foo=1; lemusic_session=abc123; bar=2"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("abc123"));
        headers.insert(header::COOKIE, HeaderValue::from_static("lemusic_session="));
        assert_eq!(session_token(&headers), None);
    }

    #[test]
    fn limiter_locks_after_failures() {
        let limiter = LoginLimiter::default();
        for _ in 0..MAX_FAILURES - 1 {
            limiter.record_failure();
        }
        assert!(limiter.locked_for().is_none());
        limiter.record_failure();
        assert!(limiter.locked_for().is_some());
        limiter.reset();
        assert!(limiter.locked_for().is_none());
    }
}
