# LeMusic 🎵

[🇩🇪 Deutsch](#deutsch) · [🇬🇧 English](#english)

---

## Deutsch

Stabiler Discord-Musikbot für **YouTube** und **SoundCloud**, geschrieben in Rust – ein Neubau von [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot). Läuft als ein Docker-Container und wird über eine Weboberfläche konfiguriert.

**Highlights**
- Keine Aussetzer: Tracks werden vollständig geladen und vorgepuffert, der nächste Track liegt bereit (Skip < 1 s)
- Selbstheilend: Retry ab letzter Position, Reconnect, abgestürzte Player starten neu
- Neustart-fest: Warteschlange und Position überleben Neustarts und Updates
- Weboberfläche mit Admin-Login: Wiedergabe, Playlists, Einstellungen, Logs, yt-dlp-Update
- Discords Ende-zu-Ende-Verschlüsselung (DAVE) wird unterstützt

**Schnellstart**

```bash
mkdir lemusic && cd lemusic
curl -fsSLO https://raw.githubusercontent.com/daranto/LeMusic-DC/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/daranto/LeMusic-DC/main/.env.example -o .env   # DISCORD_TOKEN, GUILD_ID, ADMIN_PASSWORD eintragen
docker compose up -d
```

Update: `docker compose pull && docker compose up -d` – die Musik läuft danach an derselben Stelle weiter.

Weboberfläche: http://localhost:8080 · Befehle in Discord: `/play`, `/search`, `/skip`, `/queue`, `/playlist` …

Ausführliche Anleitung (Bot anlegen, alle Befehle, Betrieb, Fehlerbehebung): **[docs/HANDBUCH.md](docs/HANDBUCH.md)**

---

## English

A reliable Discord music bot for **YouTube** and **SoundCloud**, written in Rust – a rewrite of [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot). Runs as a single Docker container and is configured through a web UI.

**Highlights**
- No dropouts: tracks are fully downloaded and pre-buffered, the next track is ready ahead of time (skip < 1 s)
- Self-healing: retries from the last position, voice reconnect, crashed players restart
- Restart-proof: queue and position survive restarts and updates
- Web UI with admin login: playback, playlists, settings, logs, one-click yt-dlp update
- Supports Discord's end-to-end voice encryption (DAVE)

**Quick start**

```bash
mkdir lemusic && cd lemusic
curl -fsSLO https://raw.githubusercontent.com/daranto/LeMusic-DC/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/daranto/LeMusic-DC/main/.env.example -o .env   # set DISCORD_TOKEN, GUILD_ID, ADMIN_PASSWORD
docker compose up -d
```

Update: `docker compose pull && docker compose up -d` – playback resumes where it left off.

Web UI: http://localhost:8080 · Discord commands: `/play`, `/search`, `/skip`, `/queue`, `/playlist` …

The bot invite link needs the scopes `bot` + `applications.commands` and the permissions View Channel, Send Messages, Embed Links, Connect and Speak (`permissions=3165184`). The detailed manual is in German: [docs/HANDBUCH.md](docs/HANDBUCH.md).

---

Images: `ghcr.io/daranto/lemusic-dc` (amd64 + arm64)

**Stack:** Rust · serenity/poise · songbird · yt-dlp · ffmpeg · axum · SQLite · Docker — License: [MIT](LICENSE)
