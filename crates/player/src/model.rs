//! Öffentliche Datentypen des Players.

use std::{sync::Arc, time::Duration};

use lemusic_core::TrackMeta;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Aktuellen Track wiederholen.
    One,
    /// Ganze Warteschlange wiederholen.
    All,
}

impl RepeatMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "aus",
            Self::One => "aktueller Track",
            Self::All => "Warteschlange",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    /// Eindeutig pro Server (für Web-Aktionen und Persistenz).
    pub id: u64,
    pub meta: Arc<TrackMeta>,
    /// Discord-User-ID; 0 = Weboberfläche bzw. Autoplaylist.
    pub requester: u64,
    /// Stammt aus der Autoplaylist. Wünsche werden immer davor eingereiht.
    #[serde(default)]
    pub autoplay: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NowPlaying {
    pub entry: QueueEntry,
    pub position: Duration,
    pub paused: bool,
    /// `false`, solange der Stream noch lädt.
    pub ready: bool,
}

/// Momentaufnahme für Befehle und Weboberfläche.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub guild_id: u64,
    pub voice_channel: Option<u64>,
    pub text_channel: Option<u64>,
    pub now: Option<NowPlaying>,
    pub queue: Vec<QueueEntry>,
    pub repeat: RepeatMode,
    pub volume: u16,
}

impl Snapshot {
    pub fn queue_duration(&self) -> Duration {
        self.queue.iter().filter_map(|e| e.meta.duration).sum()
    }
}

/// Ereignisse für Discord-Nachrichten und die Weboberfläche.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    TrackStarted {
        guild_id: u64,
        text_channel: Option<u64>,
        entry: QueueEntry,
    },
    /// Track endgültig fehlgeschlagen (nach Wiederholungen) und übersprungen.
    TrackFailed {
        guild_id: u64,
        text_channel: Option<u64>,
        entry: QueueEntry,
        error: String,
    },
    QueueFinished {
        guild_id: u64,
        text_channel: Option<u64>,
    },
    Left {
        guild_id: u64,
        text_channel: Option<u64>,
        reason: String,
    },
    /// Irgendetwas am Zustand hat sich geändert (für Live-Ansichten).
    Changed { guild_id: u64 },
}

/// Was beim Einreihen passiert ist.
#[derive(Debug, Clone)]
pub struct EnqueueOutcome {
    pub added: usize,
    /// Position des ersten neuen Tracks in der Warteschlange (1 = als Nächstes),
    /// 0 = spielt sofort.
    pub position: usize,
}

/// Anfrage zum Einreihen.
#[derive(Debug, Clone)]
pub struct EnqueueRequest {
    pub tracks: Vec<TrackMeta>,
    pub requester: u64,
    pub voice_channel: u64,
    pub text_channel: Option<u64>,
    /// Vorne statt hinten einreihen.
    pub play_next: bool,
    /// Vor diesem Eintrag einreihen (Drag & Drop in der Weboberfläche).
    /// Hat Vorrang vor `play_next`; fehlt der Eintrag, gilt die normale Regel.
    pub before: Option<u64>,
}

/// Ergebnis eines automatischen Beitritts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoJoinOutcome {
    /// Neu beigetreten; ggf. Name der gestarteten Autoplaylist.
    Joined(Option<String>),
    /// War allein in einem anderen Kanal und ist umgezogen (Wiedergabe läuft weiter).
    Moved,
    /// Schon in diesem Kanal.
    AlreadyThere,
    /// In einem anderen Kanal mit Zuhörern – bleibt dort.
    Busy,
}

/// Bearbeitung der Warteschlange über Eintrags-IDs (stabil, auch wenn
/// zwischendurch ein Track zu Ende geht).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum QueueEdit {
    Remove {
        ids: Vec<u64>,
    },
    /// Verschiebt die Einträge (Reihenfolge bleibt) vor `before`;
    /// `front` = ganz nach vorne, beides leer = ans Ende.
    Move {
        ids: Vec<u64>,
        #[serde(default)]
        before: Option<u64>,
        #[serde(default)]
        front: bool,
    },
    /// Stellt die zuletzt entfernten Einträge wieder her.
    Restore,
    /// Entfernt doppelte Titel (der erste bleibt).
    Dedupe,
    /// Entfernt alle Titel aus der Autoplaylist.
    RemoveAutoplay,
}

/// Liefert die Zahl der (menschlichen) Zuhörer in einem Sprachkanal.
/// Implementiert vom Bot über den Discord-Cache.
pub trait VoicePresence: Send + Sync + 'static {
    /// `None`, wenn unbekannt (z. B. Cache noch nicht geladen).
    fn listeners(&self, guild_id: u64, channel_id: u64) -> Option<usize>;
}
