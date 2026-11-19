//! Optionaler Audio-Cache auf der Platte.
//!
//! Vollständig geladene Titel (die komprimierte Datei von yt-dlp, ≈ 1 MB pro
//! Minute) werden unter `/data/cache` abgelegt. Beim nächsten Abspielen liest
//! ffmpeg direkt von dort – ohne YouTube/SoundCloud, sofort und auch beim
//! Spulen schnell. Wird die eingestellte Größe überschritten, fliegen die am
//! längsten nicht gespielten Titel raus (LRU über die Änderungszeit).
//!
//! Gespeichert wird nur, was yt-dlp erfolgreich (Exit-Code 0) komplett
//! geladen hat. Eine kaputte Datei wird beim nächsten Fehlversuch gelöscht
//! und neu geladen.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use lemusic_core::{SettingsStore, track_key};
use serde::Serialize;
use tracing::{debug, info, warn};

static ENABLED: AtomicBool = AtomicBool::new(false);
static MAX_BYTES: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
/// Aufräumen und Leeren nie gleichzeitig.
static MAINTENANCE: Mutex<()> = Mutex::new(());

const EXT: &str = "audio";
/// Einzelne Titel größer als dieser Anteil des Caches werden nicht gespeichert.
const MAX_ENTRY_SHARE: u64 = 4;
/// Beim Aufräumen bis auf diesen Anteil der Maximalgröße löschen (Puffer).
const EVICT_TARGET_PERCENT: u64 = 90;

/// Verzeichnis des Caches.
pub fn dir() -> PathBuf {
    lemusic_core::config::data_dir().join("cache")
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Höchstgröße eines einzelnen Eintrags (0 = Cache aus).
pub fn max_entry_bytes() -> u64 {
    if enabled() {
        MAX_BYTES.load(Ordering::Relaxed) / MAX_ENTRY_SHARE
    } else {
        0
    }
}

/// Stabiler Dateiname aus dem Track-Schlüssel (FNV-1a, unabhängig von der
/// Rust-Version) plus lesbarer Anfang zur Fehlersuche.
fn file_name(url: &str) -> String {
    let key = track_key(url);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let readable: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(40)
        .collect();
    format!("{readable}-{hash:016x}.{EXT}")
}

fn path_for(url: &str) -> PathBuf {
    dir().join(file_name(url))
}

/// Pfad der Cache-Datei, falls vorhanden (und Cache an). Markiert den Titel
/// als zuletzt benutzt.
pub fn lookup(url: &str) -> Option<PathBuf> {
    if !enabled() {
        return None;
    }
    let path = path_for(url);
    match fs::metadata(&path) {
        Ok(m) if m.is_file() && m.len() > 0 => {
            if let Ok(f) = fs::File::options().write(true).open(&path) {
                let _ = f.set_modified(SystemTime::now());
            }
            HITS.fetch_add(1, Ordering::Relaxed);
            Some(path)
        }
        _ => {
            MISSES.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// Entfernt einen (vermutlich kaputten) Eintrag.
pub fn remove(url: &str) {
    let path = path_for(url);
    if path.exists() {
        match fs::remove_file(&path) {
            Ok(()) => info!(url, "Cache-Eintrag entfernt (neu laden)"),
            Err(e) => warn!(error = %e, "Cache-Eintrag nicht löschbar"),
        }
    }
}

/// Speichert einen vollständig geladenen Titel und räumt danach auf.
pub fn store(url: &str, data: &[u8]) {
    if !enabled() || data.is_empty() || data.len() as u64 > max_entry_bytes() {
        return;
    }
    let result = (|| -> io::Result<()> {
        let dir = dir();
        fs::create_dir_all(&dir)?;
        let target = path_for(url);
        let tmp = target.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &target)
    })();
    match result {
        Ok(()) => {
            debug!(url, bytes = data.len(), "Im Cache gespeichert");
            evict();
        }
        Err(e) => warn!(error = %e, "Speichern im Cache fehlgeschlagen"),
    }
}

struct Entry {
    path: PathBuf,
    len: u64,
    used: SystemTime,
}

fn entries(dir: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let m = e.metadata().ok()?;
            match path.extension().and_then(|x| x.to_str()) {
                Some(EXT) if m.is_file() => Some(Entry {
                    len: m.len(),
                    used: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    path,
                }),
                // Reste abgebrochener Schreibvorgänge.
                Some("tmp") => {
                    let _ = fs::remove_file(&path);
                    None
                }
                _ => None,
            }
        })
        .collect()
}

/// Löscht die am längsten nicht gespielten Titel, bis die Größe passt.
pub fn evict() {
    let Ok(_guard) = MAINTENANCE.lock() else {
        return;
    };
    let max = MAX_BYTES.load(Ordering::Relaxed);
    let mut list = entries(&dir());
    let mut total: u64 = list.iter().map(|e| e.len).sum();
    if total <= max {
        return;
    }
    let target = max / 100 * EVICT_TARGET_PERCENT;
    list.sort_by_key(|e| e.used);
    let mut removed = 0;
    for e in list {
        if total <= target {
            break;
        }
        if fs::remove_file(&e.path).is_ok() {
            total = total.saturating_sub(e.len);
            removed += 1;
        }
    }
    if removed > 0 {
        info!(
            removed,
            size_mb = total / 1_048_576,
            "Cache aufgeräumt (älteste Titel entfernt)"
        );
    }
}

/// Löscht den gesamten Cache. Liefert die Zahl der gelöschten Titel.
pub fn clear() -> usize {
    let Ok(_guard) = MAINTENANCE.lock() else {
        return 0;
    };
    let removed = entries(&dir())
        .into_iter()
        .filter(|e| fs::remove_file(&e.path).is_ok())
        .count();
    info!(removed, "Cache geleert");
    removed
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub enabled: bool,
    pub entries: usize,
    pub bytes: u64,
    pub max_bytes: u64,
    /// Seit dem Start aus dem Cache gespielt / neu geladen.
    pub hits: u64,
    pub misses: u64,
}

pub fn stats() -> CacheStats {
    let list = entries(&dir());
    CacheStats {
        enabled: enabled(),
        entries: list.len(),
        bytes: list.iter().map(|e| e.len).sum(),
        max_bytes: MAX_BYTES.load(Ordering::Relaxed),
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
    }
}

/// Setzt die Cache-Einstellungen (auch für den Selbsttest per CLI).
pub fn configure(enabled: bool, max_mb: u64) {
    let was = ENABLED.swap(enabled, Ordering::Relaxed);
    let old_max = MAX_BYTES.swap(max_mb.saturating_mul(1_048_576), Ordering::Relaxed);
    if was != enabled || old_max != MAX_BYTES.load(Ordering::Relaxed) {
        info!(enabled, max_mb, "Audio-Cache-Einstellungen übernommen");
    }
    if enabled {
        // Aufräumen kann bei vielen Dateien dauern – nicht im Async-Kontext.
        std::thread::spawn(evict);
    }
}

/// Übernimmt die Audio-Einstellungen (Cache, Lautstärke angleichen) und
/// folgt späteren Änderungen.
pub fn watch_settings(settings: SettingsStore) {
    let current = settings.current();
    configure(current.cache_enabled, current.cache_max_mb);
    crate::source::set_normalize(current.normalize_volume);
    let mut rx = settings.subscribe();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let s = rx.borrow_and_update().clone();
            configure(s.cache_enabled, s.cache_max_mb);
            crate::source::set_normalize(s.normalize_volume);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_stable_and_safe() {
        let a = file_name("https://www.youtube.com/watch?v=abc123&list=x");
        assert_eq!(a, file_name("https://youtu.be/abc123"));
        assert!(a.starts_with("yt_abc123-") && a.ends_with(".audio"));
        assert!(!file_name("https://soundcloud.com/a/../../etc").contains('/'));
        assert_ne!(a, file_name("https://youtu.be/abc124"));
    }
}
