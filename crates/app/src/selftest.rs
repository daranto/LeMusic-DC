//! `lemusic selftest [--soundcloud] <Link oder Suche>` für die Kommandozeile.

use anyhow::{Result, bail};
use lemusic_player::{SearchSource, selftest};

/// So viel Audio wird im Echtzeit-Takt gelesen.
const DECODE_SECONDS: f64 = 30.0;

pub async fn run(args: &[String]) -> Result<()> {
    let (source, query) = match args {
        [flag, rest @ ..] if flag == "--soundcloud" => (SearchSource::SoundCloud, rest.join(" ")),
        rest => (SearchSource::YouTube, rest.join(" ")),
    };
    if query.is_empty() {
        bail!("Aufruf: lemusic selftest [--soundcloud] <Link oder Suche>");
    }

    let report = selftest::run(&query, source, DECODE_SECONDS).await?;
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
