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
  cp "server/target/$target/release/set-server" "$stage/"
  cp -r web/dist "$stage/web/dist"
  cat > "$stage/README.txt" <<'EOF'
SET game server

Run it from this directory, which holds the defaults it looks for
(web/dist for the frontend, set.db for the database):

  ./set-server

It listens on http://0.0.0.0:8080. Configure it with the environment variables
SET_DB, SET_STATIC, SET_ADDR, SET_BASE_URL, SET_SMTP_URL, and SET_MAIL_FROM;
without SET_SMTP_URL, emails (such as confirmation links) go to the log.
EOF
  tar -czf "dist/$name.tar.gz" -C dist "$name"
  rm -rf "$stage"
  echo "built dist/$name.tar.gz"
done
