-- Gespeicherte Playlists. Tracks als JSON-Array von TrackMeta.
CREATE TABLE playlists (
    name       TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
    tracks     TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
