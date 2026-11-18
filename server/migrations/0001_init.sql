CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
);

-- created_by is NULL for the bootstrap code printed when the database has no users.
CREATE TABLE invite_codes (
    code TEXT PRIMARY KEY,
    created_by INTEGER REFERENCES users(id),
    created_at INTEGER NOT NULL,
    used_by INTEGER REFERENCES users(id),
    used_at INTEGER
);

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
