#!/bin/sh
# Run a command in the fast-anything-dev image (docker/dev.Dockerfile) with the repository
# mounted at its own path, as the calling user. Cargo's caches and target dir live under
# target/docker*, so host and container builds never mix.
#
#   docker/dev.sh cargo test
#   docker/dev.sh sh -c 'cd examples/fuzzy-dedupe && fast-anything scan -- python3 app.py'
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
case "${1:-}" in -h|--help|'') sed -n '2,8p' "$0"; exit 0 ;; esac
tty=
[ -t 0 ] && [ -t 1 ] && tty=-t
exec docker run --rm -i $tty -u "$(id -u):$(id -g)" \
  -v "$root:$root" -w "$PWD" \
  -e HOME="$root/target/docker-home" -e CARGO_HOME="$root/target/docker-cargo" \
  -e CARGO_TARGET_DIR="$root/target/docker" -e PATH="$root/target/docker/release:/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  -e RUSTUP_HOME=/opt/rustup \
  fast-anything-dev "$@"
