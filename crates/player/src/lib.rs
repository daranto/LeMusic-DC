//! Audio: Links/Suchen auflösen, Streams erzeugen, Player pro Server.

mod actor;
mod manager;
pub mod model;
pub mod resolve;
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

/// Baut einen (lazy) Track ab Position `start_at`.
pub fn build_track(meta: Arc<TrackMeta>, volume: f32, start_at: Duration) -> Track {
    Track::new(YtDlpSource::input(meta.url.clone(), start_at)).volume(volume)
}
