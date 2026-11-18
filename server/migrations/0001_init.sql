-- email_verified_at is NULL until the user follows the confirmation link; unverified
-- users cannot log in and are replaced when someone registers the same email again.
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    email_verified_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
);

-- Single-use tokens sent by email: 'verify' confirms the address, 'reset' sets a new password.
CREATE TABLE email_tokens (
    token TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('verify', 'reset')),
    expires_at INTEGER NOT NULL
);

-- Recent requests to email each address, for the rate limit. Rows older than an hour are deleted.
CREATE TABLE email_sends (
    email TEXT NOT NULL COLLATE NOCASE,
    sent_at INTEGER NOT NULL
);
CREATE INDEX email_sends_email ON email_sends (email, sent_at);

-- Finished multiplayer games. Solo games are not recorded.
CREATE TABLE games (
    id INTEGER PRIMARY KEY,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL
);

CREATE TABLE game_players (
    game_id INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id),
    sets INTEGER NOT NULL,
    won INTEGER NOT NULL,
    PRIMARY KEY (game_id, user_id)
);
