//! Statische Dateien der Oberfläche – fest ins Binary eingebaut.

use axum::{
    http::header,
    response::{Html, IntoResponse},
};

const INDEX: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");
const DND_JS: &str = include_str!("../static/dnd.js");
const PUBLIC_HTML: &str = include_str!("../static/public.html");
const PUBLIC_JS: &str = include_str!("../static/public.js");

/// Strenge CSP: nur eigene Skripte/Styles, Bilder auch von Thumbnail-CDNs.
const CSP: &str = "default-src 'self'; img-src 'self' https: data:; style-src 'self'; \
                   script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'";

pub async fn index() -> impl IntoResponse {
    html_page(INDEX)
}

pub async fn public_index() -> impl IntoResponse {
    html_page(PUBLIC_HTML)
}

fn html_page(body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Html(body),
    )
}

pub async fn dnd_js() -> impl IntoResponse {
    script(DND_JS)
}

pub async fn public_js() -> impl IntoResponse {
    script(PUBLIC_JS)
}

pub async fn app_js() -> impl IntoResponse {
    script(APP_JS)
}

fn script(body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
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
