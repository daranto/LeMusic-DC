//! Audio: Links/Suchen auflösen, Streams erzeugen, Player pro Server.

mod actor;
pub mod diagnostics;
mod manager;
pub mod model;
pub mod resolve;
pub mod selftest;
pub mod source;

use std::{sync::Arc, time::Duration};

use lemusic_core::TrackMeta;
use songbird::tracks::Track;

pub use manager::{Player, PlayerManager};
pub use model::{
    EnqueueOutcome, EnqueueRequest, NowPlaying, PlayerEvent, QueueEntry, RepeatMode, Snapshot,
    VoicePresence,
};
pub use resolve::{MAX_PLAYLIST_TRACKS, SearchSource, is_url, resolve, search};
pub use source::YtDlpSource;

/// Gemeinsame yt-dlp-Argumente (z. B. Cookies, falls hinterlegt).
pub(crate) fn ytdlp_base_args() -> Vec<String> {
    let mut args = vec!["--no-warnings".to_string()];
    let cookies = lemusic_core::config::cookies_path();
    if cookies.is_file() {
        args.push("--cookies".into());
        args.push(cookies.display().to_string());
    }
    args
}

/// Baut einen (lazy) Track ab Position `start_at`; `attempt` > 0 nutzt die
/// langsameren, aber breiter funktionierenden yt-dlp-Standardeinstellungen.
pub fn build_track(meta: Arc<TrackMeta>, volume: f32, start_at: Duration, attempt: u8) -> Track {
    Track::new(YtDlpSource::input(meta.url.clone(), start_at, attempt)).volume(volume)
}
