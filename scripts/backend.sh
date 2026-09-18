#!/usr/bin/env bash
set -euo pipefail

source_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
runtime_dir=${OMAKINDLE_LIB_DIR:-"$HOME/.local/lib/omakindle"}
binary="$runtime_dir/omakindle-backend"
target_dir=${CARGO_TARGET_DIR:-"${XDG_CACHE_HOME:-$HOME/.cache}/omakindle/target"}

find_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    command -v cargo
    return 0
  fi
  for candidate in \
    "$HOME/.local/share/mise/shims/cargo" \
    "$HOME/.cargo/bin/cargo" \
    /usr/bin/cargo; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

build() {
  local cargo
  cargo=$(find_cargo) || {
    echo "cargo not found; install Rust (e.g. mise use -g rust@stable)" >&2
    exit 1
  }
  CARGO_TARGET_DIR="$target_dir" "$cargo" build --release --manifest-path "$source_root/backend/Cargo.toml"
  install -d -m 700 -- "$runtime_dir"
  install -m 755 -- "$target_dir/release/omakindle-backend" "$binary"
  printf 'installed %s\n' "$binary"
}

run() {
  if [ ! -x "$binary" ]; then
    echo "omakindle-backend is not installed at $binary" >&2
    echo "run: $(basename -- "$0") build" >&2
    exit 127
  fi
  exec "$binary" serve "$@"
}

case "${1:-}" in
  build) build ;;
  run) shift; run "$@" ;;
  check)
    if [ -x "$binary" ]; then
      printf 'installed: %s\n' "$binary"
    else
      printf 'missing\n'
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 build|run|check" >&2
    exit 2
    ;;
esac
