//! Statische Dateien der Oberfläche – fest ins Binary eingebaut.

use axum::{
    http::header,
    response::{Html, IntoResponse},
};

const INDEX: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

/// Strenge CSP: nur eigene Skripte/Styles, Bilder auch von Thumbnail-CDNs.
const CSP: &str = "default-src 'self'; img-src 'self' https: data:; style-src 'self'; \
                   script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'";

pub async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Html(INDEX),
    )
}

pub async fn app_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
}

pub async fn style_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        STYLE_CSS,
    )
}
