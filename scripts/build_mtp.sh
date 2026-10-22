#!/bin/sh
# Build the pinned USB dependency and helper; stdout is the native prefix only.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=1.0.30
SHA256=fea36f34f9156400209595e300840767ab1a385ede1dc7ee893015aea9c6dbaf
ARCH=$(uname -m)
case "$ARCH" in arm64|x86_64) ;; *) printf 'Unsupported architecture: %s\n' "$ARCH" >&2; exit 1 ;; esac
for tool in go clang make pkg-config curl shasum tar; do
    command -v "$tool" >/dev/null 2>&1 || { printf 'Missing MTP build prerequisite: %s\n' "$tool" >&2; exit 1; }
done
BUILD="$ROOT/target/mtp"
PREFIX="$BUILD/libusb-$VERSION-$ARCH-macos14"
ARCHIVE="$BUILD/libusb-$VERSION.tar.bz2"
SOURCE="$BUILD/source-$ARCH/libusb-$VERSION"
mkdir -p "$BUILD"
if [ ! -f "$ARCHIVE" ]; then
    curl --fail --location --retry 3 "https://github.com/libusb/libusb/releases/download/v$VERSION/libusb-$VERSION.tar.bz2" --output "$ARCHIVE.download" >&2
    mv "$ARCHIVE.download" "$ARCHIVE"
fi
printf '%s  %s\n' "$SHA256" "$ARCHIVE" | shasum -a 256 -c - >&2
if [ ! -f "$PREFIX/.tyranny-built" ]; then
    mkdir -p "$(dirname "$SOURCE")"
    tar -xjf "$ARCHIVE" -C "$(dirname "$SOURCE")"
    (
        cd "$SOURCE"
        # New SDKs expose pipe2 even when the deployment target cannot call it.
        # Force libusb's pipe+fcntl implementation for the macOS 14 minimum.
        ac_cv_func_pipe2=no MACOSX_DEPLOYMENT_TARGET=14.0 CFLAGS="-O2 -arch $ARCH -mmacosx-version-min=14.0" LDFLAGS="-arch $ARCH -mmacosx-version-min=14.0" ./configure --prefix="$PREFIX" --enable-shared --disable-static --disable-examples-build --disable-tests-build
        make -j "${MTP_BUILD_JOBS:-4}"
        make install
    ) >&2
    printf '%s\n' "$SHA256 macOS-14.0 $ARCH" > "$PREFIX/.tyranny-built"
fi
(
    cd "$ROOT/tools/mtp"
    export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
    export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
    export CGO_ENABLED=1
    export MACOSX_DEPLOYMENT_TARGET=14.0
    # cgo's cache does not key pkg-config environment changes. Include the pinned
    # prefix in its compiler/linker flags so a different native build cannot be reused.
    export CGO_CFLAGS="-O2 -I$PREFIX/include/libusb-1.0 -arch $ARCH -mmacosx-version-min=14.0"
    export CGO_LDFLAGS="-L$PREFIX/lib -arch $ARCH -mmacosx-version-min=14.0 -Wl,-headerpad_max_install_names"
    go build -mod=readonly -trimpath -o "$BUILD/tyranny-likes-smoke-mtp" .
) >&2
CREDITS="$PREFIX/share/tyranny-likes-smoke-mtp"
mkdir -p "$CREDITS"
install -m 644 "$SOURCE/COPYING" "$CREDITS/libusb-COPYING"
install -m 644 "$ARCHIVE" "$CREDITS/libusb-$VERSION.tar.bz2"
install -m 644 "$ROOT/tools/mtp/internal/mtp/LICENSE" "$CREDITS/go-mtpfs-LICENSE"
install -m 644 "$(go env GOMODCACHE)/github.com/ganeshrvel/usb@v0.0.0-20210103155855-14d96f5ae403/LICENSE" "$CREDITS/go-usb-LICENSE"
install -m 644 "$(go env GOROOT)/LICENSE" "$CREDITS/go-LICENSE"
install -m 644 "$ROOT/tools/mtp/NOTICE" "$CREDITS/NOTICE"
install -m 644 "$ROOT/scripts/build_mtp.sh" "$CREDITS/build_mtp.sh"
printf '%s\n' "$PREFIX"
