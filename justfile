# List available commands.
[default]
default:
    @just --list

# Build both components (debug; does not replace the app bundle).
build: build-server build-client

# Build both components with release optimizations (does not replace the app bundle).
build-release: build-server-release build-client-release

# Build a self-contained release app and package target/Tyranny.dmg.
dmg:
    scripts/build_dmg.sh

# Run both components' automated test suites.
test: test-server test-client

# Build the Rust server and companion command-line tools (debug).
build-server:
    prefix="$(python3 scripts/build_ffmpeg.py)" && FFMPEG_DIR="$prefix" MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --target-dir target/portable-rust --bins

# Build release Rust binaries against pinned, source-built FFmpeg.
build-server-release:
    scripts/build_release_server.sh

# Run Rust unit and integration tests (hardware tests remain opt-in).
test-server:
    prefix="$(python3 scripts/build_ffmpeg.py)" && FFMPEG_DIR="$prefix" MACOSX_DEPLOYMENT_TARGET=14.0 cargo test --locked --target-dir target/portable-rust --all-targets

# Build the SwiftUI client (debug).
build-client:
    scripts/build_mtp.sh
    swift build --package-path macos --scratch-path target/swift-client

# Build the SwiftUI client with release optimizations.
build-client-release:
    scripts/build_mtp.sh
    swift build --package-path macos --scratch-path target/swift-client --configuration release

# Run the Swift client test suite.
test-client:
    swift test --package-path macos --scratch-path target/swift-client
