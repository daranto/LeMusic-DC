# LeMusic 🎵

Stabiler Discord-Musikbot für **YouTube** und **SoundCloud**, geschrieben in Rust – ein Neubau von [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot) mit Fokus auf Ausfallsicherheit. Läuft als ein einziger Docker-Container und wird über eine Weboberfläche konfiguriert.

## Was ihn stabil macht

- **Kein Audio-Aussetzer durch Netzwerk:** Jeder Track wird von yt-dlp komplett in den Speicher geladen und vorgepuffert, bevor er startet. Der Audio-Takt (20 ms) liest nie direkt aus dem Netz.
- **Selbstheilend:** Fehlgeschlagene oder vorzeitig abgebrochene Tracks werden ab der letzten Position bis zu 2× neu versucht, hängende Streams erkannt, dann übersprungen. Abgestürzte Player werden automatisch neu gestartet.
- **Neustart-fest:** Warteschlange, Position, Lautstärke und Wiederholen liegen in SQLite. Nach einem Neustart oder Update läuft die Musik an derselben Stelle weiter.
- **Reconnect:** Bricht die Voice-Verbindung ab, verbindet sich der Bot mit Backoff neu und setzt fort.
- **Immer aktuelles yt-dlp:** Wird bei jedem Containerstart (und per Klick in der Weboberfläche) aktualisiert.
- **DAVE (Ende-zu-Ende-Verschlüsselung von Discord)** wird nativ unterstützt (songbird 0.6).
- Healthcheck, sauberes Herunterfahren, Log-Rotation, keine `unwrap()`s im Code.

## Schnellstart

### 1. Discord-Bot anlegen

1. [Discord Developer Portal](https://discord.com/developers/applications) → **New Application**
2. **Bot** → **Reset Token** → Token kopieren
3. Bot einladen (APP_ID = „Application ID“ unter *General Information*):
   ```
   https://discord.com/oauth2/authorize?client_id=APP_ID&scope=bot+applications.commands&permissions=3165184
   ```
   (Rechte: Kanäle sehen, Nachrichten senden, Links einbetten, Verbinden, Sprechen)
4. Server-ID kopieren: Discord → Einstellungen → Erweitert → *Entwicklermodus* an, dann Rechtsklick auf den Server → **Server-ID kopieren**

### 2. Konfigurieren und starten

```bash
cp .env.example .env
```

In `.env` mindestens `DISCORD_TOKEN`, `GUILD_ID` und `ADMIN_PASSWORD` eintragen, dann:

```bash
docker compose up -d --build
```

Weboberfläche: **http://localhost:8080** (Login mit `ADMIN_USER`/`ADMIN_PASSWORD`).

> Ohne `ADMIN_PASSWORD` wird beim ersten Start ein zufälliges Passwort erzeugt und einmalig ausgegeben: `docker compose logs lemusic | grep -A3 Admin-Account`

## Befehle

| Befehl | Beschreibung | Recht |
|---|---|---|
| `/play <Link oder Suche> [soundcloud] [next]` | Abspielen/Einreihen, auch Playlists | alle (`next`: DJ) |
| `/search <Suche> [soundcloud]` | 5 Treffer zur Auswahl | alle |
| `/skip` | Überspringen – direkt für DJ und Wünschenden, sonst Abstimmung | alle |
| `/forceskip`, `/skipto <Pos>` | Sofort überspringen / springen | DJ |
| `/pause`, `/resume`, `/stop` | Pause, weiter, stoppen & Kanal verlassen | DJ |
| `/seek <1:30>` | Spulen | DJ / Wünschender |
| `/volume [0–200]` | Lautstärke anzeigen/setzen | DJ |
| `/nowplaying` | Aktueller Track mit Fortschritt | alle |
| `/queue [Seite]` | Warteschlange | alle |
| `/remove <Pos>` | Track entfernen | eigene / DJ |
| `/move <von> <nach>`, `/shuffle`, `/clear` | Warteschlange bearbeiten | DJ |
| `/repeat <aus\|Track\|alle>` | Wiederholen | DJ |
| `/playlist play\|save\|add\|list\|show\|delete` | Gespeicherte Playlists | play/list/show: alle, sonst DJ |

**DJ** = Admins, Server-Manager und Mitglieder mit der DJ-Rolle. Ist keine DJ-Rolle eingestellt, sind alle DJ.

## Weboberfläche

- **Wiedergabe:** Live-Status, Cover, Fortschritt, Steuerung, Lautstärke, Spulen, Warteschlange sortieren, direkt in einen Sprachkanal abspielen
- **Playlists:** anlegen (aus der Warteschlange), bearbeiten, abspielen
- **Einstellungen:** Status-Text, Standard-Lautstärke, Inaktivitäts-Timeout, Pausieren-wenn-allein; DJ-Rolle, erlaubter Text-/Sprachkanal, Vote-Skip-Quote, maximale Track-Länge, Autoplaylist, Ansagen
- **System:** yt-dlp aktualisieren, Selbsttest, YouTube-Cookies
- **Logs** live, **Konto** (Passwort ändern)

Änderungen wirken sofort, ohne Neustart.

## Betrieb

| Aufgabe | Befehl |
|---|---|
| Logs | `docker compose logs -f` |
| Neustart (aktualisiert yt-dlp, Musik läuft weiter) | `docker compose restart` |
| Update nach Code-Änderung | `docker compose up -d --build` |
| Gesundheit | `curl localhost:8080/health` |
| Selbsttest ohne Discord | `docker compose exec lemusic lemusic selftest never gonna give you up` |
| Backup | Ordner `data/` sichern (enthält `lemusic.db`) |

### Wenn YouTube streikt

1. Weboberfläche → **System** → *yt-dlp jetzt aktualisieren* (oder `docker compose restart`)
2. **Selbsttest** ausführen
3. Meldet YouTube „Sign in to confirm you're not a bot“: mit einem **Zweit-Account** eine `cookies.txt` exportieren (Browser-Erweiterung „Get cookies.txt LOCALLY“) und unter **System → YouTube-Cookies** hochladen.

## Umgebungsvariablen

| Variable | Pflicht | Bedeutung |
|---|---|---|
| `DISCORD_TOKEN` | ✅ | Bot-Token |
| `GUILD_ID` | ✅ | ID deines Servers (Befehle werden dort sofort registriert) |
| `ADMIN_USER` / `ADMIN_PASSWORD` | empfohlen | Login der Weboberfläche (nur beim ersten Start übernommen) |
| `YTDLP_AUTO_UPDATE` | | yt-dlp beim Start aktualisieren (Standard `true`) |
| `LOG_FORMAT` | | `text` (Standard) oder `json` |
| `RUST_LOG` | | Log-Level, z. B. `info,serenity=warn,songbird=warn` |
| `WEB_BIND` / `DATA_DIR` | | Standard `0.0.0.0:8080` / `/data` |

## Architektur

```
Discord ──▶ serenity/poise (Befehle) ──▶ PlayerManager ──▶ Player-Actor je Server
                                                              │  Warteschlange, Retry,
Browser ──▶ axum (JSON-API, Login) ──────────────────────────┘  Persistenz, Reconnect
                                                              ▼
                       songbird (Voice, DAVE) ◀── PCM ◀── ffmpeg ◀── Speicher ◀── yt-dlp
```

| Crate | Aufgabe |
|---|---|
| `crates/core` | Konfiguration, SQLite, Einstellungen, Playlists, Login, Log-Puffer |
| `crates/player` | yt-dlp/ffmpeg-Quelle mit Vorpuffer, Player-Actor, Manager, Selbsttest |
| `crates/bot` | Discord-Befehle, Embeds, Rechte, Event-Weiterleitung |
| `crates/web` | Weboberfläche und API |
| `crates/app` | Start, Verdrahtung, Herunterfahren, `selftest`/`healthcheck` |

Warum nicht Lavalink? Siehe [PLAN.md](PLAN.md) – kurz: SoundCloud war dort kaputt und DAVE verursachte Aussetzer.

## Entwicklung

Voraussetzungen: Rust (rustup), `cmake`, `libopus`, Docker.

```bash
cargo clippy --all-targets
cargo test
```
