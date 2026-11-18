# SET

A multiplayer [SET](https://en.wikipedia.org/wiki/Set_(card_game)) card game. The Rust server (axum, SQLite) owns all game state and serves the TypeScript/PixiJS frontend.

## How it plays

- **Accounts.** Anyone can register with an email address, a username, and a password. The server emails a confirmation link (valid 24 hours); the account can log in once the link is followed. Players log in with their email. A forgotten password is reset with an emailed link (valid 1 hour), which also logs out every other session. Each address gets at most one email per minute and five per hour.
- **Games.** A player creates a game and becomes its leader, then invites registered players, who accept or decline in the lobby. Only the leader can start, and nobody can join after the start. A leader can also start alone; solo games are practice and are not counted in the statistics.
- **Play.** Everyone sees the same table and clicks three cards. The first valid set to reach the server scores 1 point. A wrong set locks that player out for 10 seconds. When no set is on the table, the server deals 3 more cards. The game ends when the deck is empty and no set remains; the highest score wins and ties count as a win for each tied player.
- **Disconnects.** The game continues. Reconnecting puts you back into your game with your score.
- **History.** Games played and wins per player, plus a leaderboard.

## Run with Docker

```sh
docker build -t set-game .
docker run -p 8080:8080 -v set-data:/data set-game
```

Open http://localhost:8080 and register. Without SMTP settings the confirmation link appears in the container log. To send real email, pass `-e SET_SMTP_URL=… -e SET_MAIL_FROM=… -e SET_BASE_URL=https://your.site`.

## Release archives

`scripts/package.sh` builds `dist/set-game-<version>-linux-x86_64.tar.gz` and `…-linux-aarch64.tar.gz` (pass `x86_64` or `aarch64` to build one). Each holds a statically linked `set-server` and the built frontend, so it runs on any Linux distribution. It needs npm, rustup, [zig](https://ziglang.org/download/), and `cargo install --locked cargo-zigbuild`.

On the target machine:

```sh
tar -xzf set-game-<version>-linux-<arch>.tar.gz
cd set-game-<version>-linux-<arch>
./set-server   # serves web/dist and keeps set.db in this directory
```

## Develop

From the repository root:

```sh
(cd web && npm install && npm run build)
cargo run --release --manifest-path server/Cargo.toml   # http://localhost:8080
```

For frontend work, run `npm run dev` in `web/` as well: Vite serves on :5173 and proxies the API and WebSocket to :8080.

| Variable | Default | Meaning |
|---|---|---|
| `SET_DB` | `set.db` | SQLite database file |
| `SET_STATIC` | `web/dist` | Built frontend to serve |
| `SET_ADDR` | `0.0.0.0:8080` | Listen address |
| `SET_BASE_URL` | `http://localhost:8080` | Public URL used in emailed links |
| `SET_SMTP_URL` | unset | SMTP server, such as `smtps://user:password@smtp.example.com` (port 465) or `smtp://user:password@smtp.example.com:587?tls=required`. When unset, emails are written to the server log |
| `SET_MAIL_FROM` | unset | Sender, such as `SET <set@example.com>`; required with `SET_SMTP_URL` |

## Tests

```sh
cd server && cargo test
```

The unit tests cover the rules (set validation, dealing extra cards, playing random games to the end). The integration tests run a server with several WebSocket clients: registration with email confirmation, password reset, game invitations, starting, simultaneous claims of the same set, lockout, reconnecting, results, and solo games.
