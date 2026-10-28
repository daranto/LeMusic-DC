//! Einheitliche Embeds für Antworten.

use std::time::Duration;

use lemusic_core::format_duration;
use lemusic_player::{QueueEntry, Snapshot};
use poise::serenity_prelude::{self as serenity, CreateEmbed, CreateEmbedFooter};

pub const COLOR_MAIN: u32 = 0x8b5cf6;
pub const COLOR_OK: u32 = 0x22c55e;
pub const COLOR_WARN: u32 = 0xf59e0b;
pub const COLOR_ERROR: u32 = 0xef4444;

pub fn info(text: &str) -> CreateEmbed {
    CreateEmbed::new().color(COLOR_MAIN).description(text)
}

pub fn success(text: &str) -> CreateEmbed {
    CreateEmbed::new().color(COLOR_OK).description(text)
}

pub fn warning(text: &str) -> CreateEmbed {
    CreateEmbed::new().color(COLOR_WARN).description(text)
}

pub fn error(text: &str) -> CreateEmbed {
    CreateEmbed::new()
        .color(COLOR_ERROR)
        .description(format!("⚠️ {text}"))
}

/// Discord-Markdown-Link, Sonderzeichen im Titel entschärft.
pub fn track_link(entry: &QueueEntry) -> String {
    let title: String = entry
        .meta
        .title
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .take(90)
        .collect();
    format!("[{title}]({})", entry.meta.url)
}

pub fn duration_label(d: Option<Duration>) -> String {
    d.map(format_duration).unwrap_or_else(|| "🔴 Live".into())
}

pub fn requester(entry: &QueueEntry) -> String {
    if entry.requester == 0 {
        "Autoplaylist/Web".into()
    } else {
        format!("<@{}>", entry.requester)
    }
}

/// Fortschrittsbalken wie `▬▬▬🔘▬▬▬▬▬▬`.
pub fn progress_bar(position: Duration, length: Option<Duration>) -> String {
    const WIDTH: usize = 16;
    let Some(length) = length.filter(|l| !l.is_zero()) else {
        return format!("{} / 🔴 Live", format_duration(position));
    };
    let ratio = (position.as_secs_f64() / length.as_secs_f64()).clamp(0.0, 1.0);
    let knob = ((ratio * WIDTH as f64) as usize).min(WIDTH - 1);
    let bar: String = (0..WIDTH)
        .map(|i| if i == knob { '🔘' } else { '▬' })
        .collect();
    format!(
        "{bar} `{} / {}`",
        format_duration(position),
        format_duration(length)
    )
}

pub fn now_playing(entry: &QueueEntry) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .color(COLOR_MAIN)
        .title("🎶 Spiele jetzt")
        .description(format!("**{}**\n{}", track_link(entry), entry.meta.author))
        .field("Dauer", duration_label(entry.meta.duration), true)
        .field("Gewünscht von", requester(entry), true);
    if let Some(thumb) = &entry.meta.thumbnail {
        embed = embed.thumbnail(thumb);
    }
    embed
}

pub fn now_playing_detail(snapshot: &Snapshot) -> CreateEmbed {
    let Some(now) = &snapshot.now else {
        return info("Es läuft gerade nichts.");
    };
    let state = if now.paused {
        "⏸️ Pausiert"
    } else if !now.ready {
        "⏳ Lädt …"
    } else {
        "▶️ Läuft"
    };
    let mut embed = CreateEmbed::new()
        .color(COLOR_MAIN)
        .title(state)
        .description(format!(
            "**{}**\n{}\n\n{}",
            track_link(&now.entry),
            now.entry.meta.author,
            progress_bar(now.position, now.entry.meta.duration)
        ))
        .field("Gewünscht von", requester(&now.entry), true)
        .field("Lautstärke", format!("{} %", snapshot.volume), true)
        .field("Wiederholen", snapshot.repeat.label(), true)
        .footer(CreateEmbedFooter::new(format!(
            "{} Track(s) in der Warteschlange",
            snapshot.queue.len()
        )));
    if let Some(thumb) = &now.entry.meta.thumbnail {
        embed = embed.thumbnail(thumb);
    }
    embed
}

pub const QUEUE_PAGE_SIZE: usize = 10;

pub fn queue_page(snapshot: &Snapshot, page: usize) -> CreateEmbed {
    let pages = snapshot.queue.len().div_ceil(QUEUE_PAGE_SIZE).max(1);
    let page = page.clamp(1, pages);
    let mut text = match &snapshot.now {
        Some(now) => format!(
            "**Läuft:** {} `{}`\n\n",
            track_link(&now.entry),
            duration_label(now.entry.meta.duration)
        ),
        None => "**Läuft:** nichts\n\n".into(),
    };
    if snapshot.queue.is_empty() {
        text.push_str("_Die Warteschlange ist leer._");
    }
    let start = (page - 1) * QUEUE_PAGE_SIZE;
    for (i, entry) in snapshot
        .queue
        .iter()
        .enumerate()
        .skip(start)
        .take(QUEUE_PAGE_SIZE)
    {
        text.push_str(&format!(
            "`{}.` {} `{}` · {}\n",
            i + 1,
            track_link(entry),
            duration_label(entry.meta.duration),
            requester(entry)
        ));
    }
    CreateEmbed::new()
        .color(COLOR_MAIN)
        .title("📜 Warteschlange")
        .description(text)
        .footer(CreateEmbedFooter::new(format!(
            "Seite {page}/{pages} · {} Track(s) · {} · Wiederholen: {}",
            snapshot.queue.len(),
            format_duration(snapshot.queue_duration()),
            snapshot.repeat.label()
        )))
}

pub fn mention_channel(id: u64) -> String {
    format!("<#{}>", serenity::ChannelId::new(id))
}
