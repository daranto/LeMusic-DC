//! Audioquelle: `yt-dlp` lädt den Stream, `ffmpeg` wandelt ihn in rohes
//! PCM (f32, 48 kHz, Stereo), songbird spielt es ab.
//!
//! yt-dlp übernimmt dabei alle Eigenheiten der Plattformen (YouTube-
//! Signaturen und 403-Umgehung, SoundCloud-HLS mit AAC …), ffmpeg sorgt für
//! ein einheitliches Format. Die Prozesse werden erst beim Abspielen
//! gestartet, damit Stream-URLs nie veralten.
//!
//! songbird liest Audio im Takt von 20 ms auf seinem Mixer-Thread. Blockiert
//! ein Lesevorgang, holt der Mixer die verpassten Pakete danach im
//! Schnelldurchlauf nach – Discord verwirft sie, und es fehlt Audio. Deshalb
//! pumpt ein eigener Thread den Stream in einen Puffer, und ein Track gilt
//! erst als bereit, wenn genug vorgepuffert ist.
//!
//! Zusätzlich wird die komprimierte Datei von yt-dlp vollständig in den
//! Speicher geladen (≈ 1 MB pro Minute). So ist der Download nach wenigen
//! Sekunden fertig und die Verbindung zu: Ein früh vorbereiteter Track hält
//! keine Verbindung offen, die während des laufenden Songs abreißen könnte.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use flume::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use songbird::input::{
    AudioStream, AudioStreamError, Compose, Input, RawAdapter,
    core::io::{MediaSource, ReadOnlySource},
};
use tracing::{debug, warn};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
/// Bytes pro Sekunde PCM (f32 = 4 Bytes pro Sample).
const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * CHANNELS as usize * 4;
/// Größe eines Blocks, den der Pump-Thread liest.
const CHUNK_SIZE: usize = 64 * 1024;
/// So viel Audio muss vorliegen, bevor der Track startet.
const PREBUFFER: usize = 2 * BYTES_PER_SECOND;
/// Maximaler Vorlauf im Puffer (≈ 15 s).
const BUFFER_CHUNKS: usize = 15 * BYTES_PER_SECOND / CHUNK_SIZE;
/// Obergrenze für den komprimierten Download im Speicher (darüber greift
/// Gegendruck auf yt-dlp, z. B. bei stundenlangen Mixes).
const SPOOL_LIMIT_BYTES: usize = 192 * 1024 * 1024;
/// `nice`-Wert für yt-dlp und ffmpeg (höher = niedrigere Priorität).
const HELPER_NICENESS: &str = "10";
/// Bis hierhin muss yt-dlp die ersten Daten geliefert haben (ab Anfang).
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Bei Wiederholungen großzügiger: YouTube verlangt bei den Standard-Clients
/// teils ~30 s Wartezeit vor dem Download („Sleeping … as required by the site“).
const STARTUP_TIMEOUT_RETRY: Duration = Duration::from_secs(90);
/// YouTube-Clients für den ersten Versuch: liefern ohne Zwangspause (gemessen
/// 5 s statt 35 s). Die Audioqualität (AAC) liegt über dem, was Discord
/// ohnehin überträgt (64–96 kbit/s).
const FAST_YOUTUBE_CLIENTS: &str = "youtube:player_client=tv_simply,mweb";
/// Zusatzzeit pro Minute Startposition: Für einen Einstieg mitten im Track
/// muss alles davor geladen und dekodiert werden (gemessen ~0,25 s/min).
const STARTUP_PER_MINUTE: Duration = Duration::from_secs(2);

/// Arbeitsverzeichnis für yt-dlp (temporäre Fragmente).
pub fn fragment_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("lemusic-ytdlp")
}

/// Löscht Reste abgebrochener Downloads (beim Start aufrufen).
pub fn clean_fragment_dir() {
    let dir = fragment_dir();
    if dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        warn!(error = %e, "Temp-Verzeichnis nicht aufräumbar");
    }
}

/// Lazy Quelle für eine Track-URL.
pub struct YtDlpSource {
    url: String,
    /// Startposition (für Seek und Fortsetzen nach Neustart).
    start_at: Duration,
    /// 0 = erster Versuch (schnelle Clients), danach yt-dlp-Standard.
    attempt: u8,
}

impl YtDlpSource {
    pub fn input(url: impl Into<String>, start_at: Duration, attempt: u8) -> Input {
        Input::Lazy(Box::new(Self {
            url: url.into(),
            start_at,
            attempt,
        }))
    }

    fn spawn(&self) -> io::Result<(ChildStdout, ChildGuard, LastError)> {
        // Abgebrochene HLS-Downloads hinterlassen Fragmente im
        // Arbeitsverzeichnis – daher ein eigenes Temp-Verzeichnis.
        let workdir = fragment_dir();
        std::fs::create_dir_all(&workdir)?;
        // Hilfsprogramme mit niedriger Priorität: Ihre CPU-Spitzen (yt-dlp
        // samt JS-Runtime, ffmpeg beim Vorpuffern) dürfen den Audio-Takt
        // nicht verzögern.
        let mut cmd = Command::new("nice");
        cmd.args(["-n", HELPER_NICENESS, "yt-dlp"])
            .current_dir(&workdir)
            .args(crate::ytdlp_base_args());
        if self.attempt == 0 {
            cmd.args(["--extractor-args", FAST_YOUTUBE_CLIENTS]);
        }
        let mut ytdlp = cmd
            .args([
                "--quiet",
                "--no-playlist",
                "--no-part",
                // Netzwerkaussetzer überbrücken: viele Versuche mit
                // wachsender Pause (1 s … 20 s).
                "--retries",
                "20",
                "--fragment-retries",
                "20",
                "--retry-sleep",
                "http:exp=1:20",
                "--retry-sleep",
                "fragment:exp=1:20",
                "-f",
                "bestaudio/best",
                "-o",
                "-",
                "--",
                &self.url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (Some(ytdlp_out), Some(ytdlp_err)) = (ytdlp.stdout.take(), ytdlp.stderr.take()) else {
            drop(ChildGuard(vec![ytdlp]));
            return Err(io::Error::other("yt-dlp-Pipes fehlen"));
        };
        let last_error = LastError::default();
        forward_stderr("yt-dlp", ytdlp_err, Some(last_error.clone()));

        // `-ss` nach `-i`: ffmpeg dekodiert bis zur Position und verwirft
        // alles davor. Bei einer Pipe geht es nicht anders, ist aber dank
        // vollständigem Download in den Speicher schnell.
        let start = format!("{:.3}", self.start_at.as_secs_f64());
        let ffmpeg = Command::new("nice")
            .args(["-n", HELPER_NICENESS, "ffmpeg", "-threads", "1"])
            .args(["-hide_banner", "-loglevel", "error", "-i", "pipe:0", "-vn"])
            .args(["-ss", &start])
            .args(["-f", "f32le", "-ar", &SAMPLE_RATE.to_string()])
            .args(["-ac", &CHANNELS.to_string(), "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut ffmpeg = match ffmpeg {
            Ok(child) => child,
            Err(e) => {
                drop(ChildGuard(vec![ytdlp]));
                return Err(e);
            }
        };

        let (Some(stdout), Some(stdin), Some(stderr)) = (
            ffmpeg.stdout.take(),
            ffmpeg.stdin.take(),
            ffmpeg.stderr.take(),
        ) else {
            drop(ChildGuard(vec![ytdlp, ffmpeg]));
            return Err(io::Error::other("ffmpeg-Pipes fehlen"));
        };
        forward_stderr("ffmpeg", stderr, None);

        let guard = ChildGuard(vec![ytdlp, ffmpeg]);
        spool(ytdlp_out, stdin)?;
        Ok((stdout, guard, last_error))
    }
}

#[async_trait]
impl Compose for YtDlpSource {
    /// Läuft auf einem Hilfs-Thread von songbird, nicht auf dem Mixer –
    /// hier darf also bis zum Vorpuffern blockiert werden.
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        let fail = |e: io::Error| AudioStreamError::Fail(Box::new(e));
        let (stdout, children, last_error) = self.spawn().map_err(fail)?;
        let mut stream = BufferedStream::start(stdout, children).map_err(fail)?;
        let minutes = u32::try_from(self.start_at.as_secs() / 60).unwrap_or(u32::MAX);
        let base = if self.attempt == 0 {
            STARTUP_TIMEOUT
        } else {
            STARTUP_TIMEOUT_RETRY
        };
        let timeout = base.saturating_add(STARTUP_PER_MINUTE.saturating_mul(minutes));
        if let Err(e) = stream.prebuffer(timeout) {
            // Die Fehlermeldung von yt-dlp kommt evtl. einen Moment später.
            thread::sleep(Duration::from_millis(200));
            let detail = last_error.lock().ok().and_then(|l| l.clone());
            let e = match detail {
                Some(detail) => io::Error::other(format!("{e} – {detail}")),
                None => e,
            };
            return Err(fail(e));
        }

        let raw = RawAdapter::new(ReadOnlySource::new(stream), SAMPLE_RATE, CHANNELS);
        Ok(AudioStream {
            input: Box::new(raw),
        })
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        Err(AudioStreamError::Unsupported)
    }

    fn should_create_async(&self) -> bool {
        false
    }
}

/// PCM-Strom aus dem Puffer des Pump-Threads.
struct BufferedStream {
    rx: Receiver<Vec<u8>>,
    current: Vec<u8>,
    pos: usize,
    /// Vorgepufferte Blöcke, die noch nicht gelesen wurden.
    queued: std::collections::VecDeque<Vec<u8>>,
    finished: bool,
    // Wird als Letztes gedroppt und beendet dann yt-dlp und ffmpeg.
    _children: ChildGuard,
}

impl BufferedStream {
    fn start(stdout: ChildStdout, children: ChildGuard) -> io::Result<Self> {
        let (tx, rx) = flume::bounded(BUFFER_CHUNKS.max(1));
        thread::Builder::new()
            .name("audio-pump".into())
            .spawn(move || pump(stdout, tx))?;
        Ok(Self {
            rx,
            current: Vec::new(),
            pos: 0,
            queued: std::collections::VecDeque::new(),
            finished: false,
            _children: children,
        })
    }

    /// Wartet, bis genug Audio vorliegt oder der Strom (kurz) zu Ende ist.
    fn prebuffer(&mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut buffered = 0;
        while buffered < PREBUFFER {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(chunk) => {
                    buffered += chunk.len();
                    self.queued.push_back(chunk);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.finished = true;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Stream liefert keine Daten",
                    ));
                }
            }
        }
        if buffered == 0 {
            return Err(io::Error::other(
                "Stream ist leer (yt-dlp/ffmpeg fehlgeschlagen)",
            ));
        }
        Ok(())
    }

    fn next_chunk(&mut self) -> Option<Vec<u8>> {
        if let Some(chunk) = self.queued.pop_front() {
            return Some(chunk);
        }
        if self.finished {
            return None;
        }
        match self.rx.try_recv() {
            Ok(chunk) => Some(chunk),
            Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => {
                // Puffer leer: Netzwerk ist langsamer als die Wiedergabe.
                warn!("Audio-Puffer leergelaufen – warte auf Daten");
                self.rx.recv().ok()
            }
        }
    }
}

impl Read for BufferedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.current.len() {
            match self.next_chunk() {
                Some(chunk) => {
                    self.current = chunk;
                    self.pos = 0;
                }
                None => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.current.len() - self.pos);
        buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Leitet die Fehlerausgabe eines Hilfsprogramms ins Log. Echte Fehler von
/// yt-dlp als Warnung, Wiederholungs-Meldungen und ffmpeg-Ausgaben (meist
/// „Broken pipe“ nach Skip) nur auf Debug-Level.
/// Letzte Fehlermeldung von yt-dlp (für aussagekräftige Fehler).
type LastError = Arc<std::sync::Mutex<Option<String>>>;

fn forward_stderr(program: &'static str, stderr: ChildStderr, last_error: Option<LastError>) {
    let spawned = thread::Builder::new()
        .name(format!("{program}-stderr"))
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { return };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if program == "yt-dlp" && line.starts_with("ERROR") {
                    warn!(program, "{line}");
                    if let Some(slot) = &last_error
                        && let Ok(mut slot) = slot.lock()
                    {
                        *slot = Some(line.trim_start_matches("ERROR:").trim().to_string());
                    }
                } else {
                    debug!(program, "{line}");
                }
            }
        });
    if let Err(e) = spawned {
        debug!(error = %e, "stderr-Weiterleitung nicht startbar");
    }
}

/// yt-dlp → Speicher → ffmpeg: Ein Thread lädt so schnell wie möglich in
/// den Speicher, ein zweiter füttert ffmpeg im Tempo der Wiedergabe.
fn spool(mut download: ChildStdout, mut ffmpeg_in: ChildStdin) -> io::Result<()> {
    let (tx, rx) = flume::unbounded::<Vec<u8>>();
    // Begrenzt wird nach Bytes, nicht nach Blöcken – Pipes liefern
    // unterschiedlich große Stücke.
    let queued = Arc::new(AtomicUsize::new(0));
    let queued_feed = queued.clone();
    thread::Builder::new()
        .name("audio-download".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_SIZE];
            loop {
                while queued.load(Ordering::Relaxed) > SPOOL_LIMIT_BYTES {
                    thread::sleep(Duration::from_millis(50));
                    if tx.is_disconnected() {
                        return;
                    }
                }
                match download.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        // Exakt so groß wie gelesen, sonst würde jeder
                        // kleine Lesevorgang einen ganzen Block belegen.
                        queued.fetch_add(n, Ordering::Relaxed);
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        debug!(error = %e, "Lesefehler beim Download");
                        return;
                    }
                }
            }
        })?;
    thread::Builder::new()
        .name("audio-feed".into())
        .spawn(move || {
            for chunk in rx.iter() {
                queued_feed.fetch_sub(chunk.len(), Ordering::Relaxed);
                if ffmpeg_in.write_all(&chunk).is_err() {
                    return;
                }
            }
            // Drop von `ffmpeg_in` schließt stdin → ffmpeg beendet sauber.
        })?;
    Ok(())
}

/// Liest ffmpeg-Ausgabe in den Puffer, bis der Strom endet oder der Leser
/// verschwindet (Skip/Stop).
fn pump(mut stdout: ChildStdout, tx: Sender<Vec<u8>>) {
    loop {
        let mut chunk = vec![0u8; CHUNK_SIZE];
        let mut filled = 0;
        // Blöcke möglichst voll machen, damit der Kanal nicht mit Mini-Blöcken
        // überläuft.
        while filled < CHUNK_SIZE {
            match stdout.read(&mut chunk[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    debug!(error = %e, "Lesefehler im Audio-Strom");
                    break;
                }
            }
        }
        if filled == 0 {
            return;
        }
        chunk.truncate(filled);
        let last = filled < CHUNK_SIZE;
        if tx.send(chunk).is_err() || last {
            return;
        }
    }
}

/// Beendet yt-dlp und ffmpeg, sobald der Strom nicht mehr gebraucht wird.
struct ChildGuard(Vec<Child>);

impl ChildGuard {
    fn terminate(mut children: Vec<Child>) {
        for child in &mut children {
            let _ = child.kill();
        }
        for child in &mut children {
            if let Err(e) = child.wait() {
                debug!(error = %e, "Kindprozess konnte nicht eingesammelt werden");
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let children = std::mem::take(&mut self.0);
        // `wait` blockiert – nicht auf einem Tokio-Worker ausführen.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || Self::terminate(children));
            }
            Err(_) => Self::terminate(children),
        }
    }
}
