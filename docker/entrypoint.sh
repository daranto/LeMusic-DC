#!/bin/sh
# Hält yt-dlp beim Start aktuell – YouTube ändert sich ständig, und ein
# Neustart des Containers soll reichen, um Fixes einzuspielen.
set -eu

if [ "${YTDLP_AUTO_UPDATE:-true}" = "true" ]; then
    echo "Prüfe yt-dlp auf Updates …"
    if ! timeout 120 pip install --no-cache-dir --quiet --upgrade "yt-dlp[default,deno]"; then
        echo "WARNUNG: yt-dlp-Update fehlgeschlagen, nutze vorhandene Version" >&2
    fi
fi
echo "yt-dlp $(yt-dlp --version)"

exec /usr/local/bin/lemusic "$@"
