-- Login-Sitzungen der Weboberfläche. Gespeichert wird nur der SHA-256 des
-- Tokens, nie das Token selbst.
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY NOT NULL,
    username   TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX sessions_expires ON sessions (expires_at);
