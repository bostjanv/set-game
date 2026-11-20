#!/usr/bin/env bash
# Builds release archives that run the server on Linux, one per architecture:
#   dist/set-game-<version>-linux-<arch>.tar.gz
# Each holds a static (musl) set-server binary and the built frontend in web/dist,
# so it runs on any distribution without installing anything.
#
# Usage: scripts/package.sh [x86_64] [aarch64]   (default: both)
# Needs npm, rustup, zig, and cargo-zigbuild (cargo install --locked cargo-zigbuild).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

arches=("$@")
[ ${#arches[@]} -gt 0 ] || arches=(x86_64 aarch64)
for arch in "${arches[@]}"; do
  case $arch in
    x86_64 | aarch64) ;;
    *) echo "unsupported architecture: $arch (use x86_64 or aarch64)" >&2; exit 1 ;;
  esac
done

for tool in npm rustup zig cargo-zigbuild; do
  command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

version=$(git describe --tags --always --dirty 2>/dev/null || sed -n 's/^version = "\(.*\)"/\1/p' server/Cargo.toml | head -1)

(cd web && npm ci && npm run build)

mkdir -p dist
for arch in "${arches[@]}"; do
  target=$arch-unknown-linux-musl
  rustup target add "$target"
  cargo zigbuild --release --locked --manifest-path server/Cargo.toml --target "$target"

  name=set-game-$version-linux-$arch
  stage=dist/$name
  rm -rf "$stage"
  mkdir -p "$stage/web"
  cargo_target_dir=$(cargo metadata --manifest-path server/Cargo.toml --no-deps --format-version 1 | jq -r '.target_directory')
  cp "$cargo_target_dir/$target/release/set-server" "$stage/"
  cp -r web/dist "$stage/web/dist"
  cat > "$stage/README.txt" <<'EOF'
SET game server
===============

Quick start
-----------

From this directory, run:

  ./set-server

Open http://localhost:8080 in a browser and register. Until you set up email
(see SET_SMTP_URL below), the confirmation link is printed in the server's
output: copy it into the browser to activate the account.

Stop the server with Ctrl+C. Run it from this directory: by default it serves
the frontend from web/dist and keeps its database in set.db, both relative to
the current directory.

Settings
--------

The server reads its settings from environment variables. Write them as
NAME=value in front of the command, on the same line, with no spaces around
the "=":

  SET_ADDR=0.0.0.0:3000 ./set-server

Put single quotes around a value that contains spaces or any of < > & ? ;

  SET_MAIL_FROM='SET <set@example.com>' ./set-server

To set several, list them all before the command. A backslash at the end of a
line continues the command on the next line:

  SET_SMTP_URL='smtps://set%40example.com:secret@smtp.example.com' \
  SET_MAIL_FROM='SET <set@example.com>' \
  ./set-server

All settings are optional. Each one with its default and an example:

SET_DB
  The SQLite database file; it is created if missing.
  Default: set.db
  Example: SET_DB=/var/lib/set-game/set.db

SET_STATIC
  The directory with the web frontend.
  Default: web/dist
  Example: SET_STATIC=/opt/set-game/web/dist

SET_ADDR
  The address and port to listen on. 0.0.0.0 accepts connections from other
  machines; 127.0.0.1 only from this one, such as behind a reverse proxy.
  Default: 0.0.0.0:8080
  Example: SET_ADDR=127.0.0.1:3000

SET_SMTP_URL
  The mail server for sending emails, with its user name and password. When
  unset, emails are not sent but printed in the server's output.
  Default: unset
  Example (TLS, port 465):
    SET_SMTP_URL='smtps://user:password@smtp.example.com'
  Example (STARTTLS, port 587):
    SET_SMTP_URL='smtp://user:password@smtp.example.com:587?tls=required'
  In the user name and password, write @ as %40, : as %3A, / as %2F, and
  % as %25. For example, the user name set@example.com becomes
  set%40example.com.

SET_MAIL_FROM
  The sender of the emails. Required when SET_SMTP_URL is set.
  Default: unset
  Example: SET_MAIL_FROM='SET <set@example.com>'

Running as a systemd service
----------------------------

To start the server at boot and restart it when it stops, run it as a systemd
service. The commands below need root.

1. Copy this directory to /opt/set-game:

     sudo cp -r . /opt/set-game

2. Create /etc/systemd/system/set-game.service with this content:

     [Unit]
     Description=SET game server
     After=network-online.target
     Wants=network-online.target

     [Service]
     ExecStart=/opt/set-game/set-server
     Environment=SET_STATIC=/opt/set-game/web/dist
     Environment=SET_DB=/var/lib/set-game/set.db
     EnvironmentFile=-/etc/set-game.env
     DynamicUser=yes
     StateDirectory=set-game
     Restart=on-failure

     [Install]
     WantedBy=multi-user.target

   DynamicUser runs the server as an unprivileged user that systemd creates,
   and StateDirectory gives it /var/lib/set-game for the database.

   In this file, settings go on Environment= lines. Put double quotes around
   the whole assignment when the value has spaces, and write % as %%:
     Environment="SET_MAIL_FROM=SET <set@example.com>"

3. For email, put the SMTP settings in /etc/set-game.env, so the password is
   not in the service file. Write one NAME="value" per line; % needs no
   doubling here:

     SET_SMTP_URL="smtps://set%40example.com:secret@smtp.example.com"
     SET_MAIL_FROM="SET <set@example.com>"

   Then make it readable only by root:

     sudo chmod 600 /etc/set-game.env

   Without this file, the service runs and prints emails to its log.

4. Start the service, and enable it at boot:

     sudo systemctl daemon-reload
     sudo systemctl enable --now set-game

   Check it with "systemctl status set-game", and read its output, including
   emails when SMTP is not set up, with "sudo journalctl -u set-game -f".

   After changing the service file or /etc/set-game.env, run
   "sudo systemctl daemon-reload" (only for the service file) and
   "sudo systemctl restart set-game".

To update, unpack the new archive, copy its contents over /opt/set-game, and
run "sudo systemctl restart set-game". The database in /var/lib/set-game stays.
EOF
  tar -czf "dist/$name.tar.gz" -C dist "$name"
  rm -rf "$stage"
  echo "built dist/$name.tar.gz"
done
