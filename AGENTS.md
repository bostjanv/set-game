# SET game

A multiplayer SET card game. The Rust server (`server/`: axum, sqlx with SQLite) owns all game state and serves the built TypeScript/PixiJS frontend (`web/`). README.md describes the rules of play and the environment variables.

## Commands

- Server tests: `cd server && cargo test` (unit tests for the rules, integration tests in `server/tests/game_flow.rs`).
- Frontend type-check and build: `cd web && npm run build` (runs `tsc --noEmit`, then Vite into `web/dist`).
- Run: build the frontend, then `cargo run --release --manifest-path server/Cargo.toml` from the repository root; it serves http://localhost:8080.
- Frontend development: also run `npm run dev` in `web/` (Vite on :5173 proxies `/api` and `/ws` to :8080).
- CI (`.github/workflows/ci.yml`) runs `cargo test --locked`, `npm run build`, and `docker build`. Commit `server/Cargo.lock` with dependency changes.

## Layout

- `server/src/rules.rs`: pure SET rules. A card is a number 0..81 whose base-3 digits are its attributes; `web/src/board.ts` decodes it the same way.
- `server/src/hub.rs`: live games, invitations, and connections behind one mutex, so claims apply in arrival order. Games live only in memory; finished multiplayer games are saved to the database.
- `server/src/ws.rs`: the WebSocket per logged-in user, carrying `ClientMsg`/server messages. Their TypeScript types are in `web/src/api.ts`; change both sides together.
- `server/src/api.rs`: the HTTP API: accounts, sessions, players, leaderboard.
- `server/src/auth.rs`: password hashing (argon2) and the `sid` session cookie; the `CurrentUser` extractor rejects requests without a session with 401.
- `server/src/mail.rs`: `Mailer` sends email over SMTP, writes it to the log when `SET_SMTP_URL` is unset, or keeps it in memory for tests.
- `web/src/main.ts`: the whole UI as a small client-side router that rebuilds the DOM with `h()` from `dom.ts`; `board.ts` draws the table with PixiJS.

## Accounts

- Registration and password reset use 6-digit codes sent by email (table `email_codes`, 30 minutes, 5 guesses each, single use). The request that sends a code also sets an HttpOnly cookie named after its purpose (`verify` or `reset`) holding the row's secret, and the code is accepted only with that cookie, so an intercepted email is not enough: the code must be entered in the browser that asked for it.
- `POST /api/register` creates an account with `email_verified_at` NULL and sends a code; `POST /api/verify` with `{code}` confirms the email and logs in, and `POST /api/verify/resend` sends a new code with fresh guesses. Unconfirmed accounts cannot log in and are not listed or invitable.
- Registering an email that is still unconfirmed replaces that account; unconfirmed accounts without a working code are deleted on the next registration, freeing their username.
- Login uses email and password. Usernames are only display names, unique case-insensitively.
- `POST /api/forgot` always answers 204 and sets the `reset` cookie, so it does not reveal which emails exist; for an unknown email the cookie leads to no code. Asking again replaces the code. `POST /api/reset` with `{code, password}` sets the password, confirms the email, deletes all of the user's sessions, and logs in. Its errors never say how many guesses are left, which would reveal that the account exists.
- Each address gets at most one email per minute and five per hour (`rate_limit_email`, table `email_sends`). Requests count even for unknown addresses, so a 429 from `/api/forgot` does not reveal whether an account exists. Tests that send two emails to one address within a minute call `Server::age_email_sends` in between.

## Database

The project has no production data yet. Change the schema by editing `server/migrations/0001_init.sql` directly rather than adding migrations, and delete the local database (`set.db`, `set.db-wal`, `set.db-shm`) afterwards: sqlx refuses to start with `VersionMismatch(1)` when an applied migration has changed. Once there is data to keep, add new numbered migrations instead.

## Tests

- Integration tests start a real server on a random port with a fresh database file and `Mailer::Memory`, and read codes from the in-memory outbox (`Server::code`). `Server::register` registers and confirms a user in one step.
- Test behavior through the HTTP API and WebSocket messages, as the existing tests do.
- UI changes have no automated tests: build the frontend, run the server, and check the pages in a browser.
