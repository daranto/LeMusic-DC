# LeMusic-DC – Plan

Neubau von [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot) in Rust.
Fokus: **Stabilität vor Funktionsumfang**. Betrieb in Docker, Konfiguration per Weboberfläche.

## Rahmenbedingungen (entschieden)

| Punkt | Entscheidung |
|---|---|
| Quellen | YouTube (inkl. YouTube Music, Playlists, Suche) und SoundCloud (Tracks, Sets, Suche) |
| Einsatz | Ein privater Discord-Server |
| Weboberfläche | Login mit Admin-Account (Benutzername + Passwort) |
| Befehle | Slash-Commands |
| Audio | **songbird 0.6** (Voice inkl. DAVE) + **yt-dlp** + **ffmpeg**, alles in einem Container |

### Warum diese Audio-Lösung? (Stand Oktober 2026)

Seit dem 1. März 2026 akzeptiert Discord nur noch Voice-Verbindungen mit DAVE (Ende-zu-Ende-Verschlüsselung).

- **songbird 0.6.0** (April 2026) unterstützt DAVE nativ und ist komplett in Rust.
- **yt-dlp** ist der am aktivsten gepflegte Extraktor für YouTube und SoundCloud. Er wird bei jedem Containerstart automatisch aktualisiert.
- **ffmpeg** wandelt jedes Eingangsformat in einheitliches PCM um. So kann es keine Codec-Überraschungen mehr geben.
- **Lavalink 4.2.2 wurde getestet und verworfen.** Dort sind aktuell zwei Fehler offen:
  - SoundCloud ist seit etwa Mai 2026 kaputt ([lavaplayer#189](https://github.com/lavalink-devs/lavaplayer/issues/189), selbst reproduziert).
  - Mit DAVE gibt es Audio-Aussetzer etwa einmal pro Sekunde ([Lavalink#1222](https://github.com/lavalink-devs/Lavalink/issues/1222), Fix noch nicht gemergt).
- Auch der eingebaute HTTP-Stream von songbird reicht nicht: YouTube antwortet teilweise mit 403, und den AAC-Strom von SoundCloud kann symphonia nicht lesen. Darum lädt yt-dlp den Stream selbst herunter.

**Messwerte im Container:** Ein kompletter Track von 3:33 Minuten wird von YouTube in 9,7 s geladen und umgewandelt (etwa 22× Echtzeit), von SoundCloud in 3,3 s (etwa 65× Echtzeit).

## Architektur

```
docker compose
└── lemusic (ein Container)
      ├── Rust-Binary: serenity/poise (Befehle) + songbird (Voice, DAVE)
      │     └── pro Track: yt-dlp ──pipe──▶ ffmpeg ──PCM──▶ songbird
      ├── Weboberfläche :8080 (axum)
      └── /data: SQLite, yt-dlp-Cache
```

Ein Track wird erst unmittelbar vor dem Abspielen gestartet (lazy, mit 5 s Vorladen). Dadurch kann die Stream-URL nicht ablaufen, auch nicht bei einer langen Warteschlange.

### Cargo-Workspace (ab Phase 1)

| Crate | Aufgabe |
|---|---|
| `crates/core` | Einstellungen, Datenmodell, SQLite (sqlx + Migrationen) |
| `crates/player` | GuildPlayer-Actor, Warteschlange, yt-dlp-/ffmpeg-Quelle, Wiederverbindung |
| `crates/bot` | serenity + poise: Slash-Commands, Events, Rechteprüfung |
| `crates/web` | axum + askama + htmx: Login, Einstellungen, Live-Status |
| `crates/app` | Binärdatei: Start, Verdrahtung, sauberes Herunterfahren, Selbsttest |

### Bibliotheken

| Bereich | Bibliothek |
|---|---|
| Laufzeit | `tokio` |
| Discord | `serenity` 0.12, `poise` 0.7 |
| Voice | `songbird` 0.6 (DAVE) |
| Extraktion | `yt-dlp` (mit dem Extra `deno` für YouTube), `ffmpeg` (statischer Build) |
| Web | `axum`, `askama`, `tower-sessions`, `htmx` (direkt eingebunden, kein JS-Build) |
| Datenbank | `sqlx` (SQLite) |
| Passwörter | `argon2` |
| Logging | `tracing`, `tracing-subscriber` (JSON) |
| Fehler | `thiserror`, `anyhow` |

## Stabilitätskonzept („Rock Solid“)

1. **Kein Panic im Betrieb:** `clippy::unwrap_used`, `clippy::expect_used` und `clippy::panic` werden als Fehler behandelt. Alle Fehler sind typisiert und landen im Log und im Discord-Kanal, ohne den Bot zu beenden.
2. **Actor pro Server:** Der GuildPlayer läuft als eigener Task mit einer Nachrichten-Queue. Ein Supervisor startet ihn neu, wenn er abstürzt, und die Warteschlange kommt aus der Datenbank zurück.
3. **Prozesse:** Für jeden Track laufen genau ein yt-dlp und ein ffmpeg. Bei Skip, Stop oder Drop werden beide sicher beendet, es bleiben keine Zombie-Prozesse zurück. `tini` läuft als PID 1.
4. **Voice-Abbruch:** Fällt die Voice-Verbindung weg, tritt der Bot dem Kanal automatisch wieder bei und setzt den aktuellen Track an der letzten Position fort.
5. **Track-Fehler:** Bis zu zwei Wiederholungen, danach wird der Track übersprungen und im Kanal gemeldet. Die Wiedergabe stoppt nie wegen eines einzelnen Tracks.
6. **Timeouts:** Das Auflösen eines Links ist auf 30 s begrenzt, der Voice-Beitritt auf 15 s. Ein Watchdog meldet, wenn ein Stream länger als X Sekunden keine Daten liefert.
7. **Persistenz:** Warteschlange, Position, Lautstärke und Wiederholungsmodus werden laufend in SQLite gespeichert und überstehen Neustarts und Updates.
8. **YouTube absichern:** yt-dlp wird bei jedem Start aktualisiert. Optional kommen Cookies oder ein PO-Token-Provider hinzu (in der Weboberfläche einstellbar), falls YouTube den Server-Standort blockiert.
9. **Health:** `/health` meldet den Status von Discord, Voice und Datenbank. Dazu kommen ein Docker-Healthcheck und `restart: unless-stopped`.
10. **Sauberes Herunterfahren:** Bei SIGTERM wird der Zustand gespeichert, der Bot verlässt den Voice-Kanal und beendet sich.
11. **Tests:** Ein Selbsttest (`lemusic selftest <URL>`) prüft yt-dlp und die Dekodierung ohne Discord. Dazu kommen Unit-Tests für Warteschlange und Rechte und ein Chaos-Test (Netzwerk trennen, yt-dlp abschießen).

## Funktionen

**Wiedergabe:** `/play <URL oder Suche>` (YouTube oder SoundCloud), `/pause`, `/resume`, `/skip`, `/voteskip`, `/forceskip`, `/stop`, `/seek`, `/volume`, `/repeat` (aus/einzeln/alle), `/nowplaying`

**Warteschlange:** `/queue`, `/shuffle`, `/remove`, `/move`, `/clear`

**Verwaltung:** DJ-Rolle, festgelegter Text- und Sprachkanal, Skip-Quote, maximale Track-Länge, Verlassen bei Inaktivität, Auto-Playlist, gespeicherte Playlists

**Bewusst weggelassen:** `eval`, Spotify, Lyrics (kann später ergänzt werden)

## Weboberfläche

- **Login:** Der Admin-Account (Benutzername + Passwort) wird beim ersten Start aus `ADMIN_USER` und `ADMIN_PASSWORD` angelegt und ist danach in der Oberfläche änderbar. Gespeichert mit Argon2, Session-Cookie (HttpOnly, SameSite=Strict), Schutz gegen Brute-Force-Versuche.
- **Seiten:**
  1. Dashboard: Status von Discord, Voice und yt-dlp-Version; aktueller Track mit Fortschritt; Warteschlange (live, Steuerung per Klick)
  2. Bot: Status-Text, Standardlautstärke, Inaktivitäts-Timeout, maximale Track-Länge
  3. Server: DJ-Rolle, Text- und Sprachkanal, Skip-Quote (Auswahl aus Listen, die live aus Discord kommen)
  4. Playlists und Auto-Playlist
  5. YouTube: Cookies hochladen, yt-dlp-Update auslösen, Selbsttest-Knopf
  6. Logs (live per Server-Sent Events)
  7. Account: Passwort ändern
- Änderungen werden sofort wirksam (über einen `watch`-Channel), ohne Neustart.

## Docker

- **Build:** mehrstufig (`rust:slim` + `cargo-chef`). Das Laufzeit-Image ist `python:3.13-slim` mit yt-dlp in einem venv, statischem ffmpeg, `libopus` und `tini`. Läuft ohne Root.
- **Image-Größe:** etwa 600 MB. Größte Posten: yt-dlp-venv inkl. Deno 143 MB, ffmpeg 108 MB, Bot-Binary 14 MB.
- **yt-dlp-Update:** Bei jedem Start aktualisiert der Entrypoint yt-dlp (`YTDLP_AUTO_UPDATE`). Bei einem YouTube-Problem reicht also `docker compose restart`.
- **`.env`:** `DISCORD_TOKEN`, `GUILD_ID`, `ADMIN_USER`, `ADMIN_PASSWORD`
- **Start:** `docker compose up -d`
- GitHub Action (`.github/workflows/ci.yml`): fmt, clippy, Tests und Multi-Arch-Image (amd64 und arm64) nach GHCR.

## Phasen

| # | Phase | Status | Fertig, wenn … |
|---|---|---|---|
| 0 | **Spike:** Minimal-Bot, yt-dlp-/ffmpeg-Pipeline, Selbsttest | ✅ abgeschlossen (Live-Test ok, keine Aussetzer) | Der Bot spielt im privaten Server ab |
| 1 | **Grundgerüst:** Workspace, Konfiguration, SQLite, Logging, Health | ✅ | Bot startet, Health ist grün |
| 2 | **Player-Kern:** Actor, Supervisor, Persistenz, Wiederverbindung, Watchdog | ✅ | Läuft trotz Netzwerkabbruch und Neustart weiter |
| 3 | **Befehle:** alle Befehle, DJ-Rechte, Voteskip, Seek, Playlists, Inaktivität | ✅ | Alle Funktionen sind vorhanden |
| 4 | **Weboberfläche:** Login, Einstellungen, Live-Dashboard, Logs | ✅ | Alles ist im Browser einstellbar |
| 5 | **Docker-Feinschliff:** Healthcheck, README, Image-Größe | ✅ | Ein `docker compose up -d` reicht |
| 6 | **Härtung:** Chaos-Tests, Dauerlauf mit Playlist, Speicherverbrauch prüfen | ✅ Chaos-Tests + 2 Dauerläufe · ⏳ 24-h-Lauf im Echtbetrieb | Läuft 24 h ohne Eingriff |

## Testergebnisse (Phase 6)

| Test | Ergebnis |
|---|---|
| ffmpeg mitten im Track hart beendet | Puffer spielt ~15 s weiter, Abbruch erkannt, Neustart ab gleicher Position |
| Netzwerk 30 s getrennt | Wiedergabe läuft durch, yt-dlp setzt Download fort |
| Bot-Prozess per `kill -9` | Docker startet neu, Wiedergabe ab gespeicherter Position |
| Neustart/Update (`docker compose restart`, Neuanlage) | Warteschlange, Position, Lautstärke, Wiederholen bleiben erhalten |
| Inaktivität / niemand im Kanal | Auto-Pause, Verlassen nach Timeout |
| Skip | nächster Track < 1 s (vorbereitet) |
| Seek in 2-h-Mix | 8:24 → 4 s, 20:00 → 8 s, 45:00 → 13 s |
| Echtzeit-Lesetest | 0 Blockaden, langsamster Lesevorgang < 5 ms (Takt 20 ms) |
| Dauerlauf 1 (32 min, 11 Tracks) | fand YouTube-Zwangspause (→ schnelle Clients) und altersbeschränkte Videos (→ sofort überspringen) |
| Dauerlauf 2 (30 min, 9 Tracks, mit Fixes) | 0 Ladehänger, 0 Warnungen, Health durchgehend OK, max. 128 MB |
| Speicher | 30–300 MB (Limit 1 GB) |

## Lokale Entwicklung

- Rust über `rustup` (Minimal-Profil), `cmake` über Homebrew (für libopus), Docker über OrbStack
- Prüfen: `cargo clippy`
- Selbsttest: `docker compose exec lemusic lemusic selftest [--soundcloud] [--start 1:30] <URL oder Suche>`
- Bot starten: `.env` ausfüllen, dann `docker compose up -d` (fertiges Image) bzw. lokal bauen mit `docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build`
