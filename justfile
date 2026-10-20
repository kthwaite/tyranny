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
    cargo build --locked --bins

# Build release Rust binaries against pinned, source-built FFmpeg.
build-server-release:
    scripts/build_release_server.sh

# Run Rust unit and integration tests (hardware tests remain opt-in).
test-server:
    cargo test --locked --all-targets

# Build the SwiftUI client (debug).
build-client:
    swift build --package-path macos --scratch-path target/swift-client

# Build the SwiftUI client with release optimizations.
build-client-release:
    swift build --package-path macos --scratch-path target/swift-client --configuration release

# Run the Swift client test suite.
test-client:
    swift test --package-path macos --scratch-path target/swift-client
