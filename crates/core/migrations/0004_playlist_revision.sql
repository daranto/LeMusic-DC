-- Revision für Konfliktschutz beim Bearbeiten, Quelle für „Aktualisieren“.
ALTER TABLE playlists ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE playlists ADD COLUMN source TEXT;
