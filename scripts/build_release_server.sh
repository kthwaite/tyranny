#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$#" -ne 0 ]; then
    printf '%s\n' "Usage: $0" >&2
    exit 2
fi
# FFMPEG_DIR selects the source-built headers and libraries directly; no Homebrew
# pkg-config fallback. Isolate Cargo artifacts from development FFmpeg builds.
FFMPEG_DIR=$(python3 "$ROOT/scripts/build_ffmpeg.py")
export FFMPEG_DIR
export MACOSX_DEPLOYMENT_TARGET=14.0
cargo build --locked --release --manifest-path "$ROOT/Cargo.toml" \
    --target-dir "$ROOT/target/portable-rust" --bins >&2
printf '%s\n' "$FFMPEG_DIR"
