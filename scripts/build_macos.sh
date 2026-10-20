#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$#" -gt 2 ]; then
    printf '%s\n' "Usage: $0 [debug|release] [app-path]" >&2
    exit 2
fi
CONFIGURATION=${1:-debug}
case "$CONFIGURATION" in
    debug) DEFAULT_APP="$ROOT/target/Tyranny.app" ;;
    release) DEFAULT_APP="$ROOT/target/release/Tyranny.app" ;;
    *) printf '%s\n' "Usage: $0 [debug|release] [app-path]" >&2; exit 2 ;;
esac
APP=${2:-$DEFAULT_APP}
for tool in cargo swift python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf '%s\n' "Missing build prerequisite: $tool. Install Xcode command-line tools, Rust, and Python 3." >&2
        exit 1
    fi
done
if [ "$CONFIGURATION" = release ]; then
    FFMPEG_PREFIX=$("$ROOT/scripts/build_release_server.sh")
    SERVER="$ROOT/target/portable-rust/release/tyranny-server"
else
    if ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists libavformat libavcodec libavutil libswresample; then
        printf '%s\n' "Development FFmpeg libraries are missing: brew install ffmpeg pkg-config." >&2
        exit 1
    fi
    cargo build --locked --release --manifest-path "$ROOT/Cargo.toml" --target-dir "$ROOT/target" --bin tyranny-server
    SERVER="$ROOT/target/release/tyranny-server"
fi
swift build --package-path "$ROOT/macos" --scratch-path "$ROOT/target/swift-client" --configuration "$CONFIGURATION"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/swift-client/$CONFIGURATION/Tyranny" "$APP/Contents/MacOS/Tyranny"
cp "$SERVER" "$APP/Contents/MacOS/tyranny-server"
cp "$ROOT/macos/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/macos/Resources/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
if [ "$CONFIGURATION" = release ]; then
    python3 "$ROOT/scripts/macos_dependencies.py" --bundle "$APP" --prefix "$FFMPEG_PREFIX"
    /usr/bin/ditto "$FFMPEG_PREFIX/share/tyranny-ffmpeg" "$APP/Contents/Resources/FFmpeg"
    cp "$ROOT/macos/Resources/Credits.rtf" "$APP/Contents/Resources/Credits.rtf"
    rm -f "$APP/Contents/Resources/development-dependencies.json"
else
    python3 "$ROOT/scripts/macos_dependencies.py" \
        --output "$APP/Contents/Resources/development-dependencies.json" \
        "$APP/Contents/MacOS/tyranny-server" "$APP/Contents/MacOS/Tyranny"
fi
# Signing is last: the helper and inventory are sealed into the app signature.
/usr/bin/codesign --force --sign - "$APP/Contents/MacOS/tyranny-server"
/usr/bin/codesign --force --sign - "$APP"
printf '%s\n' "$APP"
