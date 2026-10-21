# Tyranny

## Branches

- `prod` is the main branch. It only receives merges from `dev`.
- `dev` is the development and integration branch. Create every feature or fix branch from
  `dev`, and merge it back into `dev`.
- There is no `main` branch; it was retired on 2026-10-20.

## Protocol version

- A branch keeps the protocol `VERSION` it started from (`src/protocol.rs`). Bump it in the
  merge into `dev`, to one more than `dev`'s version. Other sessions branch in parallel, and
  bumping at branch start made two branches claim the same number.
- The bump also touches the copies outside `protocol.rs`:
  - `WireProtocol.version`, which `tests/swift_constants.rs` checks;
  - the `assert_eq!(VERSION, …)` test in `protocol.rs`;
  - the version defaults in `scripts/*_gate.py`;
  - version tags in `tyrannyctl` help and in doc comments.
  Grep for the old number.
