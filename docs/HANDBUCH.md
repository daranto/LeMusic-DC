# LeMusic 🎵

Stabiler Discord-Musikbot für **YouTube** und **SoundCloud**, geschrieben in Rust – ein Neubau von [jagrosh/MusicBot](https://github.com/jagrosh/MusicBot) mit Fokus auf Ausfallsicherheit. Läuft als ein einziger Docker-Container und wird über eine Weboberfläche konfiguriert.

## Was ihn stabil macht

- **Kein Audio-Aussetzer durch Netzwerk:** Jeder Track wird von yt-dlp komplett in den Speicher geladen und vorgepuffert, bevor er startet. Der Audio-Takt (20 ms) liest nie direkt aus dem Netz.
- **Selbstheilend:** Fehlgeschlagene oder vorzeitig abgebrochene Tracks werden ab der letzten Position bis zu 2× neu versucht, hängende Streams erkannt, dann übersprungen. Abgestürzte Player werden automatisch neu gestartet.
- **Neustart-fest:** Warteschlange, Position, Lautstärke und Wiederholen liegen in SQLite. Nach einem Neustart oder Update läuft die Musik an derselben Stelle weiter.
- **Reconnect:** Bricht die Voice-Verbindung ab, verbindet sich der Bot mit Backoff neu und setzt fort.
- **Immer aktuelles yt-dlp:** Wird bei jedem Containerstart, täglich zur eingestellten Uhrzeit und per Klick in der Weboberfläche aktualisiert.
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

Fertiges Image von GitHub (amd64 + arm64, z. B. auch Raspberry Pi 4/5):

```bash
mkdir lemusic && cd lemusic
curl -fsSLO https://raw.githubusercontent.com/daranto/LeMusic-DC/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/daranto/LeMusic-DC/main/.env.example -o .env
```

In `.env` mindestens `DISCORD_TOKEN`, `GUILD_ID` und `ADMIN_PASSWORD` eintragen, dann:

```bash
docker compose up -d
```

Weboberfläche: **http://localhost:8080** (Login mit `ADMIN_USER`/`ADMIN_PASSWORD`).

> Ohne `ADMIN_PASSWORD` wird beim ersten Start ein zufälliges Passwort erzeugt und einmalig ausgegeben: `docker compose logs lemusic | grep -A3 Admin-Account`

## Befehle

| Befehl | Beschreibung | Recht |
|---|---|---|
| `/play <Link oder Suche> [soundcloud] [next]` | Abspielen/Einreihen, auch Playlists | alle (`next`: DJ) |
| `/search <Suche> [soundcloud]` | 5 Treffer zur Auswahl | alle |
| `/join` | Bot in deinen Sprachkanal holen – startet die Autoplaylist, falls eine eingestellt ist und nichts läuft | alle |
| `/leave` | Kanal verlassen, Warteschlange leeren | DJ |
| `/dashboard` | Postet den Link zum öffentlichen Dashboard | alle |
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

**Wünsche vor Autoplaylist:** Läuft die Autoplaylist, kommt jeder Wunsch (Discord, öffentliches Dashboard, Weboberfläche) hinter die anderen Wünsche, aber vor die restlichen Autoplaylist-Titel – läuft nur die Autoplaylist, spielt der Wunsch also als Nächstes.

**Automatisch beitreten** (Einstellungen → Server): Betritt jemand einen Sprachkanal, kommt der Bot dazu – wie `/join`, also mit Autoplaylist, falls eingestellt. Unter „Erlaubte Sprachkanäle“ lässt sich das auf bestimmte Kanäle beschränken (keiner ausgewählt = alle; „Nur in Sprachkanal“ hat Vorrang). Regeln:
- Ist der Bot schon in einem Kanal mit Zuhörern, bleibt er dort.
- Sitzt er allein in einem anderen Kanal (z. B. pausiert, weil alle gegangen sind), zieht er um und spielt den aktuellen Titel an derselben Stelle weiter.
- Bots und der AFK-Kanal lösen nichts aus.
- Nach einem Neustart/Update ohne laufende Wiedergabe tritt er dem erlaubten Kanal bei, in dem die meisten Leute sitzen.
- Ohne Autoplaylist verlässt er den Kanal wieder nach dem Inaktivitäts-Timeout.

**yt-dlp aktuell halten:** YouTube ändert ständig etwas, und eine veraltete yt-dlp-Version ist die häufigste Ursache für „lädt nicht mehr“. Deshalb prüft der Bot einmal täglich zur eingestellten Uhrzeit (Einstellungen → Bot, Standard 04:00) auf eine neue Version und installiert sie. Das dauert wenige Sekunden, ein Neustart ist nicht nötig. Zusätzlich wird bei jedem Containerstart aktualisiert, und unter System → „Jetzt aktualisieren“ geht es sofort. Das Ergebnis der letzten Prüfung steht unter System und im Log. Die Uhrzeit gilt in der Zeitzone aus der Umgebungsvariable `TZ` (Standard Europe/Berlin). Startet der Container erst nach der Uhrzeit, zählt das Update beim Start für diesen Tag.

**Audio-Cache** (optional, Einstellungen → Bot): Vollständig geladene Titel werden unter `/data/cache` gespeichert und beim nächsten Mal direkt von dort gespielt. Sie starten dann sofort (gemessen: 10 ms statt 3–5 s), auch Spulen geht sofort, und YouTube/SoundCloud werden nicht erneut gefragt. Das hilft vor allem bei der Autoplaylist und bei Lieblingssongs.
- **Größe:** frei einstellbar (Standard 2 GB ≈ 500 Songs, ca. 1 MB pro Minute). Ist der Cache voll, werden die am längsten nicht gespielten Titel gelöscht. Einzelne Titel über ¼ der Cache-Größe (z. B. stundenlange Mixes) werden nicht gespeichert, Livestreams nie.
- **Sicher:** Gespeichert wird nur, was yt-dlp fehlerfrei komplett geladen hat. Bricht ein Titel aus dem Cache trotzdem ab, wird die Datei gelöscht und der Titel neu geladen.
- Dasselbe Video unter verschiedenen Links (`youtu.be/…`, `music.youtube.com`, `&list=…`) teilt sich einen Eintrag.
- Unter **System** siehst du Belegung, Anzahl der Titel und die Trefferquote seit dem Start. Dort kannst du den Cache auch leeren. Ausschalten behält die Dateien, nutzt sie aber nicht.

**DJ** = Admins, Server-Manager und Mitglieder mit der DJ-Rolle. Ist keine DJ-Rolle eingestellt, sind alle DJ.

## Weboberfläche

- **Wiedergabe:** Live-Status, Cover, Fortschritt, Steuerung, Lautstärke, Spulen, Warteschlange bearbeiten, direkt in einen Sprachkanal abspielen
- **Playlists:** anlegen, importieren, bearbeiten, prüfen und abspielen (bis 1000 Titel pro Playlist)
- **Einstellungen:** Status-Text, Standard-Lautstärke, Inaktivitäts-Timeout, Pausieren-wenn-allein, tägliches yt-dlp-Update (Uhrzeit), Audio-Cache (Größe); DJ-Rolle, erlaubter Text-/Sprachkanal, Vote-Skip-Quote, maximale Track-Länge, Autoplaylist, automatisch beitreten, Ansagen
- **System:** yt-dlp aktualisieren, Audio-Cache (Belegung, Trefferquote, leeren), Selbsttest, YouTube-Cookies
- **Logs** live, **Konto** (Passwort ändern)

Änderungen wirken sofort, ohne Neustart.

### Titel bearbeiten (Warteschlange und Playlists)

Warteschlange und Playlists funktionieren gleich:

| Was | Wie |
|---|---|
| Verschieben | Titel am Griff **⠿** ziehen (Maus oder Finger). Eine Linie zeigt, wo er landet. Am Bildschirmrand scrollt die Liste mit. |
| Verschieben per Tastatur | Griff anklicken bzw. mit Tab ansteuern, dann **↑ / ↓** |
| Mehrere auswählen | Häkchen setzen, mit **Shift-Klick** einen ganzen Bereich. Das Häkchen oben wählt alle (gefilterten) Titel. Ziehen nimmt die ganze Auswahl mit. |
| Auswahl bearbeiten | Leiste über der Liste: Als Nächstes, Ans Ende, Nach oben/unten, In Playlist kopieren/verschieben, Entfernen |
| Weitere Aktionen | **⋯** am Titel oder **Rechtsklick**: Jetzt spielen, Als Nächstes, Ans Ende, Link kopieren … |
| Filtern | Suchfeld über der Liste (Titel oder Interpret, mehrere Wörter möglich) |
| Rückgängig | Nach Entfernen, Leeren, Sortieren oder Duplikate-Entfernen erscheint unten rechts 10 Sekunden lang **Rückgängig** |

**Ziehen zwischen Bereichen:** Während du ziehst, erscheint unten eine Ablage-Leiste:
- Aus der **Warteschlange** → auf eine Playlist (oder „Neue Playlist“): Titel werden dort gespeichert.
- Aus einer **Playlist** → „⏭ Als Nächstes“ bzw. „➕ Warteschlange“ (sofort einreihen, ohne neues Laden über yt-dlp) oder auf eine andere Playlist (kopieren, mit gedrückter **Shift**-Taste verschieben). Andere Playlists lassen sich auch direkt in der Liste links als Ziel nutzen.

**Nur in der Warteschlange** (Menü „⋯ Mehr“): Mischen, Duplikate entfernen, alle Autoplaylist-Titel entfernen (nur die Wünsche bleiben), Als Playlist speichern, Leeren (mit Rückgängig). „▶“ am Titel spielt ihn sofort, die restliche Reihenfolge bleibt erhalten. „Bis hierhin überspringen“ springt wie früher und lässt alles davor weg.

**Nur bei Playlists** (Menü „⋯ Mehr“ der geöffneten Playlist):
- **Umbenennen** (eine eingestellte Autoplaylist zieht automatisch mit) und **Duplizieren**
- **Sortieren** nach Titel, Interpret oder Dauer, umkehren oder dauerhaft mischen (Rückgängig möglich)
- **Duplikate entfernen:** Erkennt dasselbe Video auch unter verschiedenen Link-Formen (`youtu.be/…`, `music.youtube.com`, `&list=…`).
- **Verfügbarkeit prüfen:** Testet jeden Titel im Hintergrund mit yt-dlp (zwei gleichzeitig, damit YouTube nicht drosselt; etwa 1–2 Sekunden pro Titel). Gelöschte, private oder gesperrte Videos werden rot markiert und lassen sich mit „Alle entfernen“ auf einmal löschen. Titel, die gerade nicht prüfbar waren (Netzwerk, Drosselung), werden nur gezählt, nicht markiert. Du kannst die Seite währenddessen verlassen.
- **Quelle und Aktualisieren:** Wird eine neue Playlist aus einem YouTube-/SoundCloud-Playlist-Link angelegt, merkt sie sich diesen Link. **↻ Aktualisieren** holt später nur die neu hinzugekommenen Titel (Reihenfolge und eigene Änderungen bleiben). Die Quelle lässt sich auch nachträglich festlegen, ändern oder entfernen.
- **Ab hier abspielen:** reiht die Playlist ab dem gewählten Titel ein.
- **Exportieren/Importieren:** als JSON-Datei (`*.lemusic.json`), z. B. als Backup oder zum Umziehen. Import über „Aus Datei importieren“ links.

**Konfliktschutz:** Wird eine Playlist gleichzeitig woanders geändert (zweiter Browser-Tab, `/playlist add` in Discord), überschreibt die Weboberfläche nichts. Sie lädt die Playlist neu und meldet das. Die Warteschlange arbeitet mit festen Titel-IDs, deshalb stimmt ein Verschieben auch dann, wenn zwischendurch ein Titel zu Ende geht.

**Drag & Drop von Links:** YouTube-/SoundCloud-Links (aus einem Browser-Tab, der Adressleiste oder markiertem Text) lassen sich auf „Hinzufügen“ bzw. die Warteschlange (→ einreihen), auf eine Playlist in der Liste, die geöffnete Playlist oder „Neue Playlist“ ziehen.

## Öffentliches Dashboard

Unter **`/public`** können alle, die den Link kennen, Songs wünschen: YouTube- oder SoundCloud-Links hineinziehen oder einfügen und sehen, was gerade läuft. Einrichtung unter **Einstellungen → Bot**:

- **Aktiv** schaltet es ein (Standard: aus).
- **PIN** (optional, 4 Ziffern) – Gäste müssen sie einmal eingeben (gilt 12 h). Ändert sich die PIN, sind alle Gäste abgemeldet. Nach 5 falschen Versuchen kurze Sperre.
- **Öffentliche Adresse**, z. B. `http://192.168.1.10:8080` – damit postet der Bot mit `/dashboard` den Link und hängt an jede „Spiele jetzt“-Ansage einen Knopf **🎵 Song hinzufügen**. Die PIN wird nie gepostet.

Schutz: nur YouTube-/SoundCloud-Links, höchstens 10 Links pro Wunsch und 30 pro Minute, Playlists werden auf 200 Titel gekürzt; Steuerung (Skip, Stop …) bleibt dem Admin vorbehalten. Damit andere Geräte das Dashboard erreichen, in `docker-compose.yml` den Port ohne `127.0.0.1:` freigeben (bzw. in UnRaid den Port mappen).

**Über das Internet (Reverse Proxy, Cloudflare Tunnel):** Alles, was das öffentliche Dashboard braucht (Seite, Styles, Skripte, Gast-API), liegt unter `/public/…`. Es reicht also, nur `/public` freizugeben – die Admin-Oberfläche bleibt intern. Wird die Startseite `/` über die eingetragene „Öffentliche Adresse“ aufgerufen, leitet der Bot direkt zum Dashboard weiter (intern über die IP erscheint weiter die Admin-Seite). Beispiel Cloudflare Tunnel: Path `^/(public.*)?$` (lässt `/` und `/public…` durch, blockt alles andere), Service `http://<IP>:8080`. Als „Öffentliche Adresse“ dann `https://<deine-domain>` eintragen (Umlaut-Domains in Punycode, z. B. `https://le-music.xn--gnn-sna.eu`).

## Betrieb

| Aufgabe | Befehl |
|---|---|
| Logs | `docker compose logs -f` |
| Neustart (aktualisiert yt-dlp, Musik läuft weiter) | `docker compose restart` |
| Update auf neueste Version | `docker compose pull && docker compose up -d` |
| Gesundheit | `curl localhost:8080/health` |
| Selbsttest ohne Discord | `docker compose exec lemusic lemusic selftest never gonna give you up` |
| Backup | Ordner `data/` sichern (enthält `lemusic.db`) |

### Wenn YouTube streikt

1. Weboberfläche → **System** → *yt-dlp jetzt aktualisieren* (oder `docker compose restart`)
2. **Selbsttest** ausführen
3. Meldet YouTube „Sign in to confirm you're not a bot“: mit einem **Zweit-Account** eine `cookies.txt` exportieren (Browser-Erweiterung „Get cookies.txt LOCALLY“) und unter **System → YouTube-Cookies** hochladen. Das schaltet auch **altersbeschränkte Videos** frei (sonst werden sie mit Hinweis übersprungen).

Hintergrund: Der erste Ladeversuch nutzt YouTube-Clients ohne Zwangspause (Start in ~5 s). Scheitert er, versucht der Bot es bis zu 2× mit den yt-dlp-Standardeinstellungen, die teils ~30 s Wartezeit von YouTube erfordern – vorbereitete Tracks laden das unbemerkt im Hintergrund.

### Wo der Bot laufen sollte

Discord braucht alle **20 ms** ein Audiopaket. Am besten läuft der Bot daher auf einem Rechner, der sonst wenig zu tun hat: Linux-Server, NAS, Mini-PC oder Raspberry Pi 4/5 (Image für arm64 vorhanden), per Kabel im Netz.

Auf einem normal genutzten Desktop (Browser, Discord-Client, Backups …) kann das Betriebssystem den Bot gelegentlich 15–80 ms warten lassen – das ist als kurzer Aussetzer hörbar, obwohl der Bot selbst sauber arbeitet. Der Bot misst das und meldet es im Log:

```
WARN Audio-Takt gestört – mögliche Aussetzer (CPU-Last auf dem Host) verspaetete_wecker=7 max_verspaetung_ms=82
```

Erscheint diese Meldung, liegt die Ursache beim Host, nicht bei YouTube/SoundCloud oder dem Netz.

## Umgebungsvariablen

| Variable | Pflicht | Bedeutung |
|---|---|---|
| `DISCORD_TOKEN` | ✅ | Bot-Token |
| `GUILD_ID` | ✅ | ID deines Servers (Befehle werden dort sofort registriert) |
| `ADMIN_USER` / `ADMIN_PASSWORD` | empfohlen | Login der Weboberfläche (nur beim ersten Start übernommen) |
| `YTDLP_AUTO_UPDATE` | | yt-dlp beim Start aktualisieren (Standard `true`) |
| `TZ` | | Zeitzone für das tägliche yt-dlp-Update (Standard `Europe/Berlin`) |
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

Warum nicht Lavalink? Siehe [PLAN.md](../PLAN.md) – kurz: SoundCloud war dort kaputt und DAVE verursachte Aussetzer.

## Entwicklung

Voraussetzungen: Rust (rustup), `cmake`, `libopus`, Docker.

```bash
cargo clippy --all-targets
cargo test
# Image lokal bauen und starten:
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build
# Nur die Weboberfläche ausprobieren – ohne Discord, Wiedergabe simuliert
# (http://127.0.0.1:8099, admin / demo-passwort):
cargo run -p lemusic-web --example ui_demo
# Audio-Kette ohne Discord testen (im Container), optional mit Cache:
docker run --rm -v ./data:/data --entrypoint lemusic ghcr.io/daranto/lemusic-dc selftest --cache <Link>
```

Jeder Push auf `main` baut über GitHub Actions ein neues Image (`:latest`, `:main`, `:sha-…`), Tags `v1.2.3` zusätzlich `:1.2.3`.
