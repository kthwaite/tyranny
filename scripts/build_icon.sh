#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
for tool in inkscape iconutil; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf '%s\n' "Missing icon prerequisite: $tool. brew install inkscape; iconutil ships with Xcode command-line tools." >&2
        exit 1
    fi
done
SVG="$ROOT/macos/Resources/AppIcon.svg"
ICNS="$ROOT/macos/Resources/AppIcon.icns"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SET="$WORK/AppIcon.iconset"
mkdir "$SET"
for size in 16 32 128 256 512; do
    inkscape "$SVG" --export-filename="$SET/icon_${size}x${size}.png" -w "$size" -h "$size" 2>/dev/null
    inkscape "$SVG" --export-filename="$SET/icon_${size}x${size}@2x.png" -w $((size * 2)) -h $((size * 2)) 2>/dev/null
done
iconutil --convert icns --output "$ICNS" "$SET"
printf '%s\n' "$ICNS"
