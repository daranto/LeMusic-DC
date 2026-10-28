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
    io::{self, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
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
const SPOOL_CHUNKS: usize = 256 * 1024 * 1024 / CHUNK_SIZE;
/// Bis hierhin muss yt-dlp die ersten Daten geliefert haben.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Lazy Quelle für eine Track-URL.
pub struct YtDlpSource {
    url: String,
}

impl YtDlpSource {
    pub fn input(url: impl Into<String>) -> Input {
        Input::Lazy(Box::new(Self { url: url.into() }))
    }

    fn spawn(&self) -> io::Result<(ChildStdout, ChildGuard)> {
        let mut ytdlp = Command::new("yt-dlp")
            .args([
                "--quiet",
                "--no-warnings",
                "--no-playlist",
                "--no-part",
                "-f",
                "bestaudio/best",
                "-o",
                "-",
                "--",
                &self.url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let Some(ytdlp_out) = ytdlp.stdout.take() else {
            drop(ChildGuard(vec![ytdlp]));
            return Err(io::Error::other("yt-dlp stdout fehlt"));
        };

        let ffmpeg = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i", "pipe:0", "-vn"])
            .args(["-f", "f32le", "-ar", &SAMPLE_RATE.to_string()])
            .args(["-ac", &CHANNELS.to_string(), "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();

        let mut ffmpeg = match ffmpeg {
            Ok(child) => child,
            Err(e) => {
                drop(ChildGuard(vec![ytdlp]));
                return Err(e);
            }
        };

        let (Some(stdout), Some(stdin)) = (ffmpeg.stdout.take(), ffmpeg.stdin.take()) else {
            drop(ChildGuard(vec![ytdlp, ffmpeg]));
            return Err(io::Error::other("ffmpeg-Pipes fehlen"));
        };

        let guard = ChildGuard(vec![ytdlp, ffmpeg]);
        spool(ytdlp_out, stdin)?;
        Ok((stdout, guard))
    }
}

#[async_trait]
impl Compose for YtDlpSource {
    /// Läuft auf einem Hilfs-Thread von songbird, nicht auf dem Mixer –
    /// hier darf also bis zum Vorpuffern blockiert werden.
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        let fail = |e: io::Error| AudioStreamError::Fail(Box::new(e));
        let (stdout, children) = self.spawn().map_err(fail)?;
        let mut stream = BufferedStream::start(stdout, children).map_err(fail)?;
        stream.prebuffer().map_err(fail)?;

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
    fn prebuffer(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
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

/// yt-dlp → Speicher → ffmpeg: Ein Thread lädt so schnell wie möglich in
/// den Speicher, ein zweiter füttert ffmpeg im Tempo der Wiedergabe.
fn spool(mut download: ChildStdout, mut ffmpeg_in: ChildStdin) -> io::Result<()> {
    let (tx, rx) = flume::bounded::<Vec<u8>>(SPOOL_CHUNKS);
    thread::Builder::new()
        .name("audio-download".into())
        .spawn(move || {
            loop {
                let mut chunk = vec![0u8; CHUNK_SIZE];
                match download.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => {
                        chunk.truncate(n);
                        if tx.send(chunk).is_err() {
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
