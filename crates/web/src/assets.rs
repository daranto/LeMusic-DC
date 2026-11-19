//! Statische Dateien der Oberfläche – fest ins Binary eingebaut.

use axum::{
    http::header,
    response::{Html, IntoResponse},
};

const INDEX: &str = include_str!("../static/index.html");
/// Die Admin-Oberfläche liegt in mehreren Dateien unter `static/app/` und
/// wird als ein Skript ausgeliefert – Reihenfolge und gemeinsamer
/// Gültigkeitsbereich bleiben damit genau wie in einer Datei.
const APP_JS: &str = concat!(
    include_str!("../static/app/00-core.js"),
    include_str!("../static/app/10-player.js"),
    include_str!("../static/app/20-playlists.js"),
    include_str!("../static/app/30-settings.js"),
    include_str!("../static/app/40-system.js"),
);
const STYLE_CSS: &str = include_str!("../static/style.css");
const DND_JS: &str = include_str!("../static/dnd.js");
const TRACKLIST_JS: &str = include_str!("../static/tracklist.js");
const PUBLIC_HTML: &str = include_str!("../static/public.html");
const PUBLIC_JS: &str = include_str!("../static/public.js");

/// Strenge CSP: nur eigene Skripte/Styles, Bilder auch von Thumbnail-CDNs.
const CSP: &str = "default-src 'self'; img-src 'self' https: data:; style-src 'self'; \
                   script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
                   object-src 'none'; form-action 'self'";

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

pub async fn tracklist_js() -> impl IntoResponse {
    script(TRACKLIST_JS)
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
