# Tagging Review Minors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Close the five minors parked by the final review of the MusicBrainz tagging feature
(TSK-sooty-peony-srffg).

**Architecture:** Three independent tasks with disjoint files. Task 1 changes the proposal, apply
and revert logic in `src/tagging/engine.rs`. Task 2 changes one client message. Task 3 makes server
shutdown finish any re-index that a tag write queued.

**Spec:** `docs/superpowers/specs/2026-10-21-musicbrainz-tags-design.md` (binding). Its anti-goal
applies throughout: never damage a file, and never lose a value the user did not see.

## Global Constraints

- Work only in `/Users/kit/Source/_p.music/tyranny-tagging-minors` on `fix/tagging-review-minors`
  (cut from `dev` at 16fa207). It merges into `dev`.
- No protocol change. `VERSION` stays 16. No new dependencies.
- Tests first. A Rust test lives next to the code it tests (`src/tagging/tests.rs`,
  `src/library/tests.rs`). A Swift test uses swift-testing in `macos/Tests/TyrannyLikesSmokeCoreTests`.
- Commits: one plain imperative sentence. No attribution or Co-Authored-By lines. Stage by
  explicit path. Never stage `.claude/`. Other tasks commit at the same time: retry on
  `.git/index.lock`.
- Gates:
  - Rust tasks: `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
    `cargo test --locked --lib`.
  - The Swift task: from `macos/`, `swift build -Xswiftc -warnings-as-errors`,
    `swift build -c release -Xswiftc -warnings-as-errors` and `swift test`.
  - The controller runs the full gates before the merge.

---

### Task 1: Proposal note, kept partners on omit, second-home revert check

**Files:**
- Modify: `src/tagging/engine.rs` (`propose_file` ~680-752, `prepare` ~819-870,
  `revert_file` ~887)
- Test: `src/tagging/tests.rs`

1. **Note for dropped partners.** `propose_file` sets `row.removes_other_copies` from
   `copies_of(&changed)` only (~752). It must also be true when
   `tags.partners_dropped_with(&changed)` is non-empty. Example: a secondary APE `Track` item
   holding `/12`, with the primary tag already holding the total. The review must then show
   "Also removes old copies of these fields from other tags".
   - Test `dropped_partner_alone_shows_the_removal_note`.
2. **Omitting an added partner keeps its value.** In `prepare`, suppose a field is in `omitted`,
   the written fields would drop a secondary copy of it (`partners_dropped_with`), and the
   primary tag has no value for it.
   - Then write that field to the primary tag with the dropped copy's value, instead of losing
     it.
   - Journal it like any written field: `before` absent, `after` the copy value. The dropped copy
     goes in `removed`.
   - Test `omitting_an_added_partner_keeps_the_copied_value`. Fixture: an MP3 with an ID3v2 tag
     without a total and an APE `Track=3/12`. Apply TrackNumber with `omit_fields: [track_total]`.
     The file then reads total 12, and revert restores APE `Track=3/12` exactly.
3. **Revert checks the second home.** `revert_file` compares only the primary values with
   "after". For the ID3 recording ID, also compare the second home: the MusicBrainz-owner UFID,
   written together with `TXXX:MusicBrainz Track Id`. If it differs from "after", the file is
   "changed since the edit" and is skipped, and nothing is written.
   - Test `revert_skips_a_file_whose_ufid_changed_after_the_edit`. Apply, change only the UFID
     with lofty, revert. The file is skipped and the UFID keeps the outside value.

### Task 2: Edit-list timeout message

**Files:**
- Modify: `macos/Sources/TyrannyLikesSmokeCore/Tagging.swift` (`TagEditMessage`)
- Modify: `macos/Sources/TyrannyLikesSmoke/Models/TaggingModel.swift` (`refreshEdits`)
- Test: `macos/Tests/TyrannyLikesSmokeCoreTests/TaggingTests.swift`

- `refreshEdits` maps a `tag_edits` failure through `TagLookupMessage`, so a timeout reads
  "MusicBrainz took too long to answer.". That message is wrong for a journal read.
- Add `TagEditMessage.listText(for:)`. A `request_timeout` gives "Recent tag edits took too long
  to load. Try again."; every other code falls back to `TagLookupMessage.text(for:)`.
- `refreshEdits` uses `listText`.
- Test `editListTimeoutMessage`: the timeout gives the new text, and a `lookup` failure falls back
  unchanged.

### Task 3: Shutdown finishes queued re-indexes

**Files:**
- Modify: `src/tagging/mod.rs` (`Handle`: track detached re-index tasks)
- Modify: `src/owner.rs` (`Owner::shutdown` order)
- Modify: `src/library/mod.rs` (`Library::shutdown` / coordinator exit drains path-scoped work)
- Test: `src/library/tests.rs`, `src/tagging/tests.rs`

Today `Owner::shutdown` stops tagging, then the owner thread shuts the library down, and only then
joins tagging. A re-index that a finished write queued is lost, because the library's Reindex
queue lives only in memory. The fix:

1. **Tagging.** `Handle` counts its detached apply/revert tasks (the `tokio::spawn` at
   mod.rs ~243). Add `pub async fn settle(&self, limit: Duration)`, which waits until the count is
   0 or `limit` passes.
2. **Owner.** `Owner::shutdown` runs, in order:
   1. `tagging.stop()`;
   2. join the tagging worker (`tagging.shutdown()`), so an apply in flight finishes its current
      file and replies;
   3. `tagging.settle(Duration::from_secs(5))`;
   4. send `Work::Shutdown`, which shuts the library down.

   Keep every other shutdown step as it is. Note: `shutdown(self)` consumes the workers, so split
   it or clone the handle as needed.
3. **Library.** When the coordinator is asked to shut down, it finishes path-scoped work first:
   - an active `ScanScope::Paths` scan, then every `pending_reindex` path, with the existing
     machinery;
   - the drain is bounded at 10 s in total, and anything left is dropped with an `eprintln!`;
   - a Root scan in progress is still interrupted, as today.
4. **Tests:**
   - `shutdown_finishes_a_queued_reindex` (library): queue a Reindex for a changed file, shut down
     at once, reopen the library on the same database, and the track holds the new metadata.
   - `settle_waits_for_a_detached_apply` (tagging): an apply's re-index completes before `settle`
     returns.

## Review Focus

1. **A quit during a large apply.** It finishes the current file, journals it, and re-indexes
   everything written before the process exits, within the bounds above.
2. **Revert after another tool edited only the UFID.** The file is skipped and no value is lost.
3. **A protocol client omitting an added partner.** The value is kept in the primary tag and stays
   revertible.
