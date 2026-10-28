#!/bin/sh
# Start des Containers:
# 1. (als root) Schreibrechte für das Datenverzeichnis herstellen und auf den
#    Benutzer „lemusic“ ohne Root-Rechte wechseln.
# 2. yt-dlp aktualisieren – YouTube ändert sich ständig, ein Neustart des
#    Containers soll reichen, um Fixes einzuspielen.
set -eu

DATA="${DATA_DIR:-/data}"

if [ "$(id -u)" = "0" ]; then
    # Docker legt einen fehlenden Bind-Mount-Ordner (./data) als root an.
    mkdir -p "$DATA"
    if ! chown -R lemusic:lemusic "$DATA" 2>/dev/null; then
        echo "WARNUNG: Rechte von $DATA nicht änderbar (z. B. Netzlaufwerk)." >&2
    fi
    exec setpriv --reuid=lemusic --regid=lemusic --init-groups -- "$0" "$@"
fi

if [ ! -w "$DATA" ]; then
    echo "FEHLER: $DATA ist für Benutzer $(id -u) nicht beschreibbar." >&2
    echo "Auf dem Host beheben mit: sudo chown -R $(id -u):$(id -g) <Pfad zu data>" >&2
    exit 1
fi

if [ "${YTDLP_AUTO_UPDATE:-true}" = "true" ]; then
    echo "Prüfe yt-dlp auf Updates …"
    if ! timeout 120 pip install --no-cache-dir --quiet --upgrade "yt-dlp[default,deno]"; then
        echo "WARNUNG: yt-dlp-Update fehlgeschlagen, nutze vorhandene Version" >&2
    fi
fi
echo "yt-dlp $(yt-dlp --version)"

exec /usr/local/bin/lemusic "$@"
