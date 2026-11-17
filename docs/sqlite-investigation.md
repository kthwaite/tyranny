# SQLite correctness investigation

Base: `dev` at `dc561b7`. This branch (`investigation/sqlite-correctness`) contains the search machinery and merges the independently reviewable fixes. No live library is required. In a worktree, use a worktree-local Cargo target directory: sharing a target directory between concurrent checkouts can select a stale test executable. Check that each filtered command actually ran a test.

```sh
export FFMPEG_DIR="$(python3 scripts/build_ffmpeg.py)" MACOSX_DEPLOYMENT_TARGET=14.0
export CARGO_TARGET_DIR="$PWD/target/sqlite-investigation" PROPTEST_CASES=256
cargo test --locked --lib ranges_remove_exactly_the_displayed_tracks
cargo test --locked --lib sqlite_removal_matches_display_for_every_source
cargo test --locked --lib membership_properties
cargo test --locked --lib snapshot_search
cargo test --locked --lib sqlite_report_links_and_listening_history_share_a_snapshot
cargo test --locked --lib grouping::history_tests
cargo test --locked --lib sqlite_failure_leaves_committed_prefix_then_retries_once
cargo test --locked --lib occurrence_histories -- --nocapture
```

- **Range removal.** Compare displayed Recent and Library rows with an age-based membership model. Remove a selected suffix with holes, with/without a first-row anchor, and compare the exact set of excluded assets. The original failure shrank to yesterday's one visible Recent row: selection `[0,1)` resolved to nothing. See `proptest-regressions/library/sqlite_removal_properties.txt` and `fix/sqlite-recent-removal`. Ages are deliberately separated from the moving clock boundary; this is not a test of a selection crossing the 30-day boundary between requests.
- **History.** Replay generated open, count, checkpoint and close operations against a Vec model. Interrupt a write with either `RAISE(ABORT)` before the revision update or a deferred foreign key that fails at `COMMIT`. Compare the committed prefix and the final replayed history/track statistics. This does not simulate filesystem or power failure. Calibration: omitting the increment on a repeated indexed track was caught at seed `1414`, with two counted plays but aggregate count one.
- **Selection variants.** A 36-case matrix crosses all nine list sources with contiguous/fragmented ranges and unavailable-row exclusions. Accepting a selection must remove exactly its distinct track IDs, even with repeated playlist occurrences; rejecting one must leave the count and revision untouched.
- **Statistics links.** Retaining rows for unavailable assets is necessary for history, but those rows cannot contribute to visible-library names or counts. The history-based membership model and 81-state enumeration compare links and multiplicities after exclusion, disappearance and return. See `fix/stats-visible-library-links`.
- **Report snapshots.** Schedule a WAL writer between the revision query and subsequent report queries without timing or sleeps. Compare Stats/Wrapped tuples and Years membership against committed states. The `snapshot_search` module persists both seeds; `fix/stats-wal-snapshot` contains three standalone regressions.
- **Grouping.** Reconcile two roots after scan, retag, missing-file and exclusion histories. Compare visible partitions and summaries to both independent raw-tag classification and a rebuild. Two distinct repairs: `fix/sqlite-excluded-release` preserves old release keys as rescan seeds, and `fix/sqlite-release-summary` repairs a title no visible member supplies. The suite does not require a particular winner when live tags disagree.
- **Playlist occurrences.** `investigation/sqlite-playlists` at `ccefd1143f` passed 512 histories at seed `874211`, including 399 order-changing moves with nonmonotonic requests, 928 retired-handle references, 398 late rollbacks and 374 histories removing a track through duplicate occurrences. Wrong move ordering and omission of the save-time availability check were both detected by deliberate mutations. Three distinct tracks, up to 40 edits; no 500-row boundary, live playback or concurrent writers in this model.

Repository-wide investigation log and individual triggers: the **SQLite correctness, 2026-11-17** section of the vault's *Tyranny property-testing investigation*.
