-- Einstellungen als JSON-Dokumente pro Schlüssel (z. B. "bot").
-- Neue Felder brauchen so keine Migration: fehlende Felder bekommen beim
-- Laden ihren Standardwert.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
