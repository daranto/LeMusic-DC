# LeMusic 🎵

[🇩🇪 Deutsch](#deutsch) · [🇬🇧 English](#english)

---

## Deutsch

Stabiler Discord-Musikbot für **YouTube** und **SoundCloud**, geschrieben in Rust – ein Neubau von [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot). Läuft als ein Docker-Container und wird über eine Weboberfläche konfiguriert.

**Stabilität**
- Keine Aussetzer: Tracks werden vollständig geladen und vorgepuffert, der nächste Track liegt bereit (Skip < 1 s)
- Selbstheilend: Retry ab letzter Position, Reconnect, abgestürzte Player starten neu; tote Videos werden erkannt und übersprungen
- Neustart-fest: Warteschlange und Position überleben Neustarts und Updates
- Discords Ende-zu-Ende-Verschlüsselung (DAVE) wird unterstützt
- Optionaler **Audio-Cache** mit einstellbarer Größe: oft gespielte Titel starten sofort und werden nicht erneut von YouTube/SoundCloud geladen

**Funktionen**

*Discord*
- `/play`, `/search`, `/skip` (mit Abstimmung), `/queue`, `/seek`, `/repeat`, `/volume`, `/playlist` …
- `/join` holt den Bot in deinen Kanal und startet die Autoplaylist; `/dashboard` postet den Link zum öffentlichen Dashboard
- **Automatisch beitreten:** Betritt jemand einen (erlaubten) Sprachkanal, kommt der Bot dazu; sitzt er woanders allein, zieht er samt laufendem Titel um
- „Spiele jetzt“-Ansagen im festgelegten Kanal, mit Knopf **🎵 Song hinzufügen**

*Weboberfläche (Admin-Login)*
- Wiedergabe live steuern: Pause, Skip, Spulen, Lautstärke, Wiederholen, direkt in einen Sprachkanal abspielen
- **Warteschlange und Playlists bearbeiten wie in einem Musikplayer:**
  - Titel per **Drag & Drop** verschieben (Maus, Touch und Tastatur)
  - **Mehrfachauswahl** mit Shift-Klick: als Nächstes, ans Ende, in eine Playlist kopieren/verschieben, entfernen
  - **Rückgängig** nach Entfernen, Leeren, Sortieren
  - **Filter** für große Listen, Kontextmenü per Rechtsklick
  - Titel aus einer Playlist direkt in die Warteschlange oder in eine andere Playlist ziehen und umgekehrt
- **Playlists** (bis 1000 Titel):
  - aus YouTube-/SoundCloud-Playlists importieren und später **aktualisieren** (holt nur neue Titel)
  - umbenennen, duplizieren, sortieren, Duplikate entfernen, ab einem bestimmten Titel abspielen
  - **Verfügbarkeit prüfen:** findet gelöschte, private oder gesperrte Videos, die sich dann mit einem Klick entfernen lassen
  - Export/Import als JSON-Datei
  - Konfliktschutz: Gleichzeitige Änderungen (zweiter Tab, Discord) überschreiben sich nicht
- **Autoplaylist:** läuft, wenn niemand etwas wünscht; Wünsche spielen immer vor den Autoplaylist-Titeln
- Einstellungen, Logs, **tägliches automatisches yt-dlp-Update** zur einstellbaren Uhrzeit (plus Update per Klick), Selbsttest, YouTube-Cookies

*Öffentliches Dashboard (optional mit 4-stelliger PIN)*
- Gäste sehen, was läuft, und wünschen Songs per Drag & Drop oder Einfügen von YouTube-/SoundCloud-Links
- Läuft auch hinter Cloudflare Tunnel/Reverse Proxy: alles liegt unter `/public`, die Admin-Seite bleibt intern
- Schutz: nur YouTube/SoundCloud, Limits pro Minute und für wartende Gast-Wünsche, PIN-Sperre pro Absender, keine Steuerung für Gäste

**Schnellstart**

```bash
mkdir lemusic && cd lemusic
curl -fsSLO https://raw.githubusercontent.com/daranto/LeMusic-DC/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/daranto/LeMusic-DC/main/.env.example -o .env   # DISCORD_TOKEN, GUILD_ID, ADMIN_PASSWORD eintragen
docker compose up -d
```

Update: `docker compose pull && docker compose up -d` – die Musik läuft danach an derselben Stelle weiter.

Weboberfläche: http://localhost:8080 · Befehle in Discord: `/play`, `/join`, `/search`, `/skip`, `/queue`, `/playlist`, `/dashboard` …

Ausführliche Anleitung (Bot anlegen, alle Befehle, Betrieb, Fehlerbehebung): **[docs/HANDBUCH.md](docs/HANDBUCH.md)**

---

## English

A reliable Discord music bot for **YouTube** and **SoundCloud**, written in Rust – a rewrite of [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot). Runs as a single Docker container and is configured through a web UI.

**Reliability**
- No dropouts: tracks are fully downloaded and pre-buffered, the next track is ready ahead of time (skip < 1 s)
- Self-healing: retries from the last position, voice reconnect, crashed players restart; dead videos are detected and skipped
- Restart-proof: queue and position survive restarts and updates
- Supports Discord's end-to-end voice encryption (DAVE)
- Optional **audio cache** with a configurable size: frequently played tracks start instantly and aren't fetched from YouTube/SoundCloud again

**Features**

*Discord*
- `/play`, `/search`, `/skip` (with voting), `/queue`, `/seek`, `/repeat`, `/volume`, `/playlist` …
- `/join` summons the bot and starts the autoplaylist; `/dashboard` posts the public dashboard link
- **Auto-join:** when someone enters an (allowed) voice channel the bot follows; if it sits alone elsewhere, it moves over and keeps playing the current track
- "Now playing" announcements in a configured channel, with a **🎵 Song hinzufügen** ("add a song") button

*Web UI (admin login)*
- Live playback control: pause, skip, seek, volume, repeat, play straight into a voice channel
- **Edit the queue and playlists like a music player:**
  - **drag & drop** to reorder (mouse, touch and keyboard)
  - **multi-select** with shift-click: play next, move to end, copy/move to a playlist, remove
  - **undo** after removing, clearing or sorting
  - **filter** for large lists, right-click context menu
  - drag tracks from a playlist into the queue or another playlist, and vice versa
- **Playlists** (up to 1000 tracks):
  - import from YouTube/SoundCloud playlists and **refresh** later (fetches only new tracks)
  - rename, duplicate, sort, remove duplicates, play from a given track
  - **availability check:** finds deleted, private or blocked videos so you can remove them in one click
  - JSON export/import
  - conflict protection: simultaneous edits (second tab, Discord) never overwrite each other
- **Autoplaylist:** plays when nobody requests anything; requests always play before autoplaylist tracks
- Settings, live logs, **daily yt-dlp auto-update** at a configurable time (plus one-click update), self-test, YouTube cookies

*Public dashboard (optional 4-digit PIN)*
- Guests see what's playing and request songs by dragging or pasting YouTube/SoundCloud links
- Works behind Cloudflare Tunnel/reverse proxies: everything lives under `/public`, the admin UI stays internal
- Protected: YouTube/SoundCloud only, rate limits and a cap on pending guest requests, per-sender PIN lockout, no playback control for guests

**Quick start**

```bash
mkdir lemusic && cd lemusic
curl -fsSLO https://raw.githubusercontent.com/daranto/LeMusic-DC/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/daranto/LeMusic-DC/main/.env.example -o .env   # set DISCORD_TOKEN, GUILD_ID, ADMIN_PASSWORD
docker compose up -d
```

Update: `docker compose pull && docker compose up -d` – playback resumes where it left off.

Web UI: http://localhost:8080 · Discord commands: `/play`, `/join`, `/search`, `/skip`, `/queue`, `/playlist`, `/dashboard` …

The bot invite link needs the scopes `bot` + `applications.commands` and the permissions View Channel, Send Messages, Embed Links, Connect and Speak (`permissions=3165184`). The detailed manual is in German: [docs/HANDBUCH.md](docs/HANDBUCH.md).

---

Images: `ghcr.io/daranto/lemusic-dc` (amd64 + arm64)

**Stack:** Rust · serenity/poise · songbird · yt-dlp · ffmpeg · axum · SQLite · Docker — License: [MIT](LICENSE)
