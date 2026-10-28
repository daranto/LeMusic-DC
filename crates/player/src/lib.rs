//! Audio: Links/Suchen auflösen, Streams erzeugen, Warteschlange bedienen.

pub mod resolve;
pub mod source;

use std::sync::Arc;

use songbird::tracks::{Track, TrackQueue};

pub use resolve::{MAX_PLAYLIST_TRACKS, SearchSource, TrackMeta, resolve};
pub use source::YtDlpSource;

/// Baut einen abspielbereiten (lazy) Track mit angehängten Metadaten.
pub fn build_track(meta: Arc<TrackMeta>, volume: f32) -> Track {
    Track::new_with_data(YtDlpSource::input(meta.url.clone()), meta).volume(volume)
}

/// Metadaten eines Tracks aus der Warteschlange.
///
/// Alle Tracks werden über [`build_track`] erzeugt, tragen also immer
/// `TrackMeta`.
pub fn meta_of(handle: &songbird::tracks::TrackHandle) -> Arc<TrackMeta> {
    handle.data::<TrackMeta>()
}

/// Bereitet den nächsten Track in der Warteschlange vor (yt-dlp starten,
/// herunterladen, vorpuffern), damit Skip und Übergänge ohne Wartezeit sind.
pub fn prepare_next(queue: &TrackQueue) {
    if let Some(next) = queue.current_queue().get(1) {
        // Ist der Track schon bereit, passiert nichts.
        let _ = next.make_playable();
    }
}
