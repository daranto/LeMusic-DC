//! Selbsttest ohne Discord: löst eine Anfrage auf, öffnet den Audiostream
//! und dekodiert einige Sekunden. Beweist, dass yt-dlp und die Codecs im
//! Container funktionieren.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use songbird::input::{Input, LiveInput, codecs};

use crate::{
    resolve::{self, SearchSource},
    source::YtDlpSource,
};

/// So viel Audio wird im Echtzeit-Takt gelesen.
const DECODE_SECONDS: f64 = 30.0;
/// Ab dieser Lesedauer würde der Mixer seinen 20-ms-Takt verpassen.
const STALL_THRESHOLD: Duration = Duration::from_millis(10);

pub async fn run(args: &[String]) -> Result<()> {
    let (source, query) = match args {
        [flag, rest @ ..] if flag == "--soundcloud" => (SearchSource::SoundCloud, rest.join(" ")),
        rest => (SearchSource::YouTube, rest.join(" ")),
    };
    if query.is_empty() {
        bail!("Aufruf: lemusic selftest [--soundcloud] <Link oder Suche>");
    }

    let started = Instant::now();
    let tracks = resolve::resolve(&query, source).await?;
    println!(
        "aufgelöst: {} Track(s) in {:?}",
        tracks.len(),
        started.elapsed()
    );
    let first = tracks.first().ok_or_else(|| anyhow!("keine Tracks"))?;
    println!(
        "  → {} – {} ({:?})",
        first.title, first.author, first.duration
    );

    let started = Instant::now();
    let input = YtDlpSource::input(first.url.clone())
        .make_playable_async(codecs::get_codec_registry(), codecs::get_probe())
        .await
        .map_err(|e| anyhow!("Stream konnte nicht geöffnet werden: {e}"))?;
    println!("Stream geöffnet in {:?}", started.elapsed());

    let Input::Live(LiveInput::Parsed(mut parsed), _) = input else {
        bail!("Stream wurde nicht geparst");
    };
    let params = parsed.decoder.codec_params().clone();
    println!(
        "Codec: {:?}, {} Hz, {:?} Kanäle",
        params.codec,
        params.sample_rate.unwrap_or_default(),
        params.channels.map(|c| c.count())
    );

    // Wie der songbird-Mixer: im 20-ms-Takt lesen und messen, ob ein
    // Lesevorgang blockiert. Jede Blockade wäre ein hörbarer Aussetzer.
    let sample_rate = f64::from(params.sample_rate.unwrap_or(48_000));
    let mut decoded_frames: u64 = 0;
    let mut slowest = Duration::ZERO;
    let mut stalls = 0u32;
    let started = Instant::now();
    while (decoded_frames as f64) / sample_rate < DECODE_SECONDS {
        let read_start = Instant::now();
        let packet = parsed.format.next_packet()?;
        if packet.track_id() != parsed.track_id {
            continue;
        }
        decoded_frames += parsed.decoder.decode(&packet)?.frames() as u64;
        let took = read_start.elapsed();
        slowest = slowest.max(took);
        if took > STALL_THRESHOLD {
            stalls += 1;
        }
        let due = Duration::from_secs_f64(decoded_frames as f64 / sample_rate);
        std::thread::sleep(due.saturating_sub(started.elapsed()));
    }
    println!(
        "{:.1} s im Echtzeit-Takt gelesen · langsamster Lesevorgang {:?} · Blockaden > {:?}: {}",
        decoded_frames as f64 / sample_rate,
        slowest,
        STALL_THRESHOLD,
        stalls
    );
    if stalls > 0 {
        bail!("Wiedergabe würde aussetzen");
    }
    println!("OK");
    Ok(())
}
