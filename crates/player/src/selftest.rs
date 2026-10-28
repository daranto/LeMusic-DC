//! Selbsttest ohne Discord: löst eine Anfrage auf, öffnet den Audiostream
//! und liest einige Sekunden im Takt des songbird-Mixers. Beweist, dass
//! yt-dlp, ffmpeg und die Pufferung funktionieren.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use songbird::input::{Input, LiveInput, codecs};

use crate::{SearchSource, YtDlpSource, resolve};

/// Ab dieser Lesedauer würde der Mixer seinen 20-ms-Takt verpassen.
pub const STALL_THRESHOLD: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Serialize)]
pub struct SelfTestReport {
    pub title: String,
    pub author: String,
    pub url: String,
    pub resolve_ms: u128,
    pub open_ms: u128,
    pub decoded_secs: f64,
    pub slowest_read_ms: f64,
    /// Lesevorgänge über [`STALL_THRESHOLD`] – jeder wäre ein Aussetzer.
    pub stalls: u32,
}

impl SelfTestReport {
    pub fn ok(&self) -> bool {
        self.stalls == 0
    }
}

pub async fn run(query: &str, source: SearchSource, seconds: f64) -> Result<SelfTestReport> {
    let started = Instant::now();
    let tracks = resolve(query, source).await?;
    let resolve_ms = started.elapsed().as_millis();
    let first = tracks
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("keine Tracks"))?;

    let started = Instant::now();
    let input = YtDlpSource::input(first.url.clone(), Duration::ZERO)
        .make_playable_async(codecs::get_codec_registry(), codecs::get_probe())
        .await
        .map_err(|e| anyhow!("Stream konnte nicht geöffnet werden: {e}"))?;
    let open_ms = started.elapsed().as_millis();

    let Input::Live(LiveInput::Parsed(mut parsed), _) = input else {
        bail!("Stream wurde nicht geparst");
    };

    // Echtzeit-Lesen blockiert (sleep) – auf einen eigenen Thread.
    let (decoded_secs, slowest, stalls) = tokio::task::spawn_blocking(move || {
        let sample_rate = f64::from(parsed.decoder.codec_params().sample_rate.unwrap_or(48_000));
        let mut frames: u64 = 0;
        let mut slowest = Duration::ZERO;
        let mut stalls = 0u32;
        let started = Instant::now();
        while (frames as f64) / sample_rate < seconds {
            let read_start = Instant::now();
            let packet = match parsed.format.next_packet() {
                Ok(p) => p,
                // Stream zu Ende (kurzer Track) ist kein Fehler.
                Err(_) if frames > 0 => break,
                Err(e) => return Err(anyhow!("Lesefehler: {e}")),
            };
            if packet.track_id() != parsed.track_id {
                continue;
            }
            frames += parsed.decoder.decode(&packet)?.frames() as u64;
            let took = read_start.elapsed();
            slowest = slowest.max(took);
            if took > STALL_THRESHOLD {
                stalls += 1;
            }
            let due = Duration::from_secs_f64(frames as f64 / sample_rate);
            std::thread::sleep(due.saturating_sub(started.elapsed()));
        }
        Ok((frames as f64 / sample_rate, slowest, stalls))
    })
    .await??;

    Ok(SelfTestReport {
        title: first.title,
        author: first.author,
        url: first.url,
        resolve_ms,
        open_ms,
        decoded_secs,
        slowest_read_ms: slowest.as_secs_f64() * 1000.0,
        stalls,
    })
}
