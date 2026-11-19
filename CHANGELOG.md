# Änderungen

Alle nennenswerten Änderungen an LeMusic. Versionen folgen
[Semantic Versioning](https://semver.org/lang/de/); jede Version gibt es als
Docker-Image `ghcr.io/daranto/lemusic-dc:<version>` (z. B. `0.2.0` oder
`0.2` für die neueste 0.2.x), `latest` folgt dem Stand von `main`.

## [0.2.0] – unveröffentlicht

### Sicherheit
- Öffentliches Dashboard: höchstens 100 wartende Gast-Wünsche, 100 Titel pro
  Minute (Playlists zählen mit allen Titeln); Warteschlange insgesamt höchstens
  2000 Wünsche.
- PIN-Sperre pro Absender mit wachsender Wartezeit (1 min → 1 h) statt einer
  Sperre für alle; globale Obergrenze, aufhebbar durch PIN-Änderung.
- Cookies über HTTPS mit `Secure`; Sicherheits-Header (`nosniff`, CSP,
  `X-Frame-Options`, …) auf allen Antworten.
- SQLite auf 3.54 (`libsqlite3-sys` 0.40) wegen CVE-2026-119375.

### Betrieb
- `/health` prüft zusätzlich Player-Actor, Voice-Wiedergabe und yt-dlp.
- yt-dlp-Updates werden geprüft; funktioniert die neue Version nicht, geht es
  automatisch auf die vorherige zurück.
- Backup per Klick (System → Backup herunterladen).
- `/metrics` im Prometheus-Format.

### Entwicklung
- Player-Actor und Admin-Oberfläche in übersichtliche Module aufgeteilt.
- HTTP-Integrationstests für Login, CSRF, PIN-Sperre, Gast-Limits,
  Sicherheits-Header, Health, Metriken und Backup.
- CI: `cargo deny` (Sicherheitsmeldungen, Lizenzen, Herkunft), wöchentlicher
  Lauf, JavaScript-Syntaxprüfung, Dependabot, SBOM und Herkunftsnachweis für
  die Images, Image-Tags `<major>.<minor>`.

## [0.1.0] – 2026-11-19

Erste Version (vor der Einführung von Versionsnummern; Stand vor #1).

- Discord-Musikbot in Rust: songbird 0.6 (DAVE), yt-dlp, ffmpeg; YouTube und
  SoundCloud inkl. Playlists und Suche.
- Stabilität: vollständiges Vorpuffern, vorbereiteter nächster Track,
  Wiederholung ab letzter Position, Reconnect, Watchdog, Zustand übersteht
  Neustarts; in Chaos- und Dauertests geprüft.
- Befehle: `/play`, `/search`, `/skip` (Abstimmung), `/queue`, `/seek`,
  `/repeat`, `/volume`, `/playlist`, `/join`, `/leave`, `/dashboard` …
- Weboberfläche mit Admin-Login: Wiedergabe, Warteschlange und Playlists mit
  Drag & Drop, Mehrfachauswahl, Rückgängig, Verfügbarkeitsprüfung, Import aus
  YouTube/SoundCloud; Einstellungen, Logs, Selbsttest, YouTube-Cookies.
- Autoplaylist; Wünsche spielen vor ihr. Automatisches Beitreten.
- Öffentliches Dashboard unter `/public` (optional mit PIN).
- Tägliches yt-dlp-Update, optionaler Audio-Cache.
- Docker-Image für amd64 und arm64.

[0.2.0]: https://github.com/daranto/LeMusic-DC/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/daranto/LeMusic-DC/releases/tag/v0.1.0
