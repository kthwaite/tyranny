#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
for tool in cargo swift python3 pkg-config; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf '%s\n' "Missing build prerequisite: $tool. Install Xcode command-line tools, Rust, Python 3, and brew install ffmpeg pkg-config." >&2
        exit 1
    fi
done
if ! pkg-config --exists libavformat libavcodec libavutil libswresample; then
    printf '%s\n' "FFmpeg development libraries are missing: brew install ffmpeg pkg-config, then rerun scripts/build_macos.sh." >&2
    exit 1
fi
# Pin the output location even when the developer has CARGO_TARGET_DIR set.
cargo build --locked --release --manifest-path "$ROOT/Cargo.toml" --target-dir "$ROOT/target" --bin tyranny-server
swift build --package-path "$ROOT/macos" --scratch-path "$ROOT/target/swift-client"
APP="$ROOT/target/Tyranny.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/swift-client/debug/Tyranny" "$APP/Contents/MacOS/Tyranny"
cp "$ROOT/target/release/tyranny-server" "$APP/Contents/MacOS/tyranny-server"
cp "$ROOT/macos/Info.plist" "$APP/Contents/Info.plist"
python3 "$ROOT/scripts/macos_dependencies.py" \
    --output "$APP/Contents/Resources/development-dependencies.json" \
    "$APP/Contents/MacOS/tyranny-server" "$APP/Contents/MacOS/Tyranny"
# Signing is last: the helper and inventory are sealed into the app signature.
/usr/bin/codesign --force --sign - "$APP/Contents/MacOS/tyranny-server"
/usr/bin/codesign --force --sign - "$APP"
printf '%s\n' "$APP"
