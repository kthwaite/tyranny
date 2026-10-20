//! The Swift client hard-codes a few protocol constants. These tests read the Swift sources and
//! fail when a constant drifts from the server's value.
use std::{fs, path::Path};
use tyranny::{
    protocol::{MAX_STATION_BATCH, VERSION},
    server::MAX_REQUEST_BYTES,
};

/// The value of `static let <name>` in the Swift type declared by `owner` (e.g. `enum
/// WireProtocol`). The value may be an integer literal or a product such as `16 * 1024`.
fn swift_constant(file: &str, owner: &str, name: &str) -> u64 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let start = source
        .find(owner)
        .unwrap_or_else(|| panic!("{file} does not declare `{owner}`"));
    let declaration = format!("static let {name}");
    let line = source[start..]
        .lines()
        .find(|line| line.trim_start().starts_with("public ") && line.contains(&declaration))
        .unwrap_or_else(|| panic!("`{owner}` in {file} has no `{declaration}`"));
    let (_, value) = line
        .split_once('=')
        .unwrap_or_else(|| panic!("{file}: `{}` has no value", line.trim()));
    value
        .split('*')
        .map(|factor| {
            factor
                .trim()
                .replace('_', "")
                .parse::<u64>()
                .unwrap_or_else(|_| {
                    panic!(
                        "{file}: `{}` is not an integer literal or product",
                        line.trim()
                    )
                })
        })
        .product()
}

#[test]
fn swift_wire_protocol_version_matches_server() {
    let swift = swift_constant(
        "macos/Sources/TyrannyCore/WireModels.swift",
        "enum WireProtocol",
        "version",
    );
    assert_eq!(
        swift,
        u64::from(VERSION),
        "Swift WireProtocol.version is {swift}, but protocol::VERSION is {VERSION}; bump both together"
    );
}

#[test]
fn swift_station_batch_limit_matches_server() {
    let swift = swift_constant(
        "macos/Sources/TyrannyCore/StationCatalog.swift",
        "class StationCatalog",
        "maximumBatch",
    );
    assert_eq!(
        swift, MAX_STATION_BATCH as u64,
        "Swift StationCatalog.maximumBatch is {swift}, but protocol::MAX_STATION_BATCH is {MAX_STATION_BATCH}"
    );
}

#[test]
fn swift_request_frame_limit_matches_server() {
    let swift = swift_constant(
        "macos/Sources/TyrannyCore/CommandTransport.swift",
        "enum RequestFrame",
        "limit",
    );
    assert_eq!(
        swift, MAX_REQUEST_BYTES as u64,
        "Swift RequestFrame.limit is {swift}, but server::MAX_REQUEST_BYTES is {MAX_REQUEST_BYTES}"
    );
}
