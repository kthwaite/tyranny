#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$#" -ne 0 ]; then
    printf '%s\n' "Usage: $0" >&2
    exit 2
fi
mkdir -p "$ROOT/target"
STAGING=$(mktemp -d "$ROOT/target/dmg.XXXXXX")
trap 'rm -rf "$STAGING"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Assemble a fresh bundle without touching the development app or running helper.
mkdir "$STAGING/contents"
APP="$STAGING/contents/tyranny likes smoke.app"
"$ROOT/scripts/build_macos.sh" release "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
ln -s /Applications "$STAGING/contents/Applications"

# Publish only a complete, verified image; preserve the previous DMG on failure.
/usr/bin/hdiutil create -volname "tyranny likes smoke" -srcfolder "$STAGING/contents" \
    -format UDZO -fs HFS+ "$STAGING/tyranny likes smoke.dmg"
/usr/bin/hdiutil verify "$STAGING/tyranny likes smoke.dmg"
mv -f "$STAGING/tyranny likes smoke.dmg" "$ROOT/target/tyranny likes smoke.dmg"
printf '%s\n' "$ROOT/target/tyranny likes smoke.dmg" \
    "Self-contained native libraries; ad-hoc signed, not notarized."
