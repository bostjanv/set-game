# SET

A multiplayer [SET](https://en.wikipedia.org/wiki/Set_(card_game)) card game. The Rust server (axum, SQLite) owns all game state and serves the TypeScript/PixiJS frontend.

## How it plays

- **Registration is by invitation only.** Any player can create single-use invite codes on their profile page and share the link. When the database has no players yet, the server logs a bootstrap link for the first account.
- **Games.** A player creates a game and becomes its leader, then invites registered players, who accept or decline in the lobby. Only the leader can start, and nobody can join after the start. A leader can also start alone; solo games are practice and are not counted in the statistics.
- **Play.** Everyone sees the same table and clicks three cards. The first valid set to reach the server scores 1 point. A wrong set locks that player out for 3 seconds. When no set is on the table, the server deals 3 more cards. The game ends when the deck is empty and no set remains; the highest score wins and ties count as a win for each tied player.
- **Disconnects.** The game continues. Reconnecting puts you back into your game with your score.
- **History.** Games played and wins per player, plus a leaderboard.

## Run with Docker

```sh
docker build -t set-game .
docker run -p 8080:8080 -v set-data:/data set-game
```

Open http://localhost:8080 and use the `/register?code=…` link from the container log to create the first account.

## Develop

From the repository root:

```sh
(cd web && npm install && npm run build)
cargo run --release --manifest-path server/Cargo.toml   # http://localhost:8080
```

For frontend work, run `npm run dev` in `web/` as well: Vite serves on :5173 and proxies the API and WebSocket to :8080.

| Variable     | Default        | Meaning                     |
|--------------|----------------|-----------------------------|
| `SET_DB`     | `set.db`       | SQLite database file        |
| `SET_STATIC` | `web/dist`     | Built frontend to serve     |
| `SET_ADDR`   | `0.0.0.0:8080` | Listen address              |

## Tests

```sh
cd server && cargo test
```

The unit tests cover the rules (set validation, dealing extra cards, playing random games to the end). The integration tests run a server with several WebSocket clients: invite-only registration, invitations, starting, simultaneous claims of the same set, lockout, reconnecting, results, and solo games.
