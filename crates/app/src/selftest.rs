//! `lemusic selftest [--soundcloud] <Link oder Suche>` für die Kommandozeile.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use lemusic_core::parse_duration;
use lemusic_player::{SearchSource, selftest};

/// So viel Audio wird im Echtzeit-Takt gelesen.
const DECODE_SECONDS: f64 = 30.0;

pub async fn run(args: &[String]) -> Result<()> {
    let usage = "Aufruf: lemusic selftest [--soundcloud] [--start 1:30] <Link oder Suche>";
    let mut source = SearchSource::YouTube;
    let mut start_at = Duration::ZERO;
    let mut rest = args.iter();
    let mut words = Vec::new();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--soundcloud" => source = SearchSource::SoundCloud,
            "--start" => {
                start_at = rest
                    .next()
                    .and_then(|v| parse_duration(v))
                    .ok_or_else(|| anyhow!(usage))?;
            }
            word => words.push(word),
        }
    }
    let query = words.join(" ");
    if query.is_empty() {
        bail!(usage);
    }

    let started = Instant::now();
    let report = selftest::run(&query, source, start_at, DECODE_SECONDS).await?;
    println!("Gesamt:       {:?}", started.elapsed());
    println!("Track:        {} – {}", report.title, report.author);
    println!("Auflösen:     {} ms", report.resolve_ms);
    println!("Stream offen: {} ms", report.open_ms);
    println!(
        "Gelesen:      {:.1} s im Echtzeit-Takt · langsamster Lesevorgang {:.1} ms · Blockaden > {:?}: {}",
        report.decoded_secs,
        report.slowest_read_ms,
        selftest::STALL_THRESHOLD,
        report.stalls
    );
    if !report.ok() {
        bail!("Wiedergabe würde aussetzen");
    }
    println!("OK");
    Ok(())
}
