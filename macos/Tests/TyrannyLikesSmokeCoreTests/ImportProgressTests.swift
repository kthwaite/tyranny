import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct ImportProgressTests {
    private func scan(_ phase: String, root: UInt64? = 7, discovered: Int = 0, processed: Int = 0,
                      total: Int? = nil, failures: Int = 0, skipped: Int = 0,
                      error: String? = nil) -> ScanStatus {
        ScanStatus(root_id: root, phase: phase, discovered: discovered, processed: processed,
                   total: total, updated: 0, unchanged: 0, failures: failures, skipped: skipped,
                   last_error: error)
    }

    private func library(_ scan: ScanStatus, roots: Int = 1, tracks: Int = 0,
                         durationMs: UInt64 = 0, bytes: UInt64 = 0) -> LibraryState {
        LibraryState(revision: 1, roots: roots, tracks: tracks, duration_ms: durationMs,
                     size_bytes: bytes, scan: scan)
    }

    private func session(baseline: ScanStatus? = nil) -> ImportSession {
        ImportSession(rootID: 7, baseline: baseline)
    }

    @Test func idleIsPreparing() {
        var s = session()
        let p = s.progress(library: library(scan("idle")))
        #expect(p.stage == .preparing)
        #expect(p.title == "Importing Your Music")
        #expect(p.detail == nil)
        #expect(p.fraction == nil)
    }

    @Test func statusForAnotherRootIsPreparing() {
        var s = session()
        let p = s.progress(library: library(scan("indexing", root: 9, processed: 5, total: 10)))
        #expect(p.stage == .preparing)
    }

    @Test func discoveringShowsFoundCount() {
        var s = session()
        let p = s.progress(library: library(scan("discovering", discovered: 1204)))
        #expect(p.stage == .discovering)
        #expect(p.detail == "Finding music… 1,204 files found")
        #expect(p.fraction == nil)
    }

    @Test func indexingShowsFraction() {
        var s = session()
        let p = s.progress(library: library(scan("indexing", processed: 412, total: 1129)))
        #expect(p.stage == .indexing)
        #expect(p.detail == "Reading 412 of 1,129 files")
        #expect(p.fraction == 412.0 / 1129.0)
    }

    @Test func indexingWithNilTotalIsIndeterminate() {
        var s = session()
        let p = s.progress(library: library(scan("indexing", processed: 412, total: nil)))
        #expect(p.detail == "Reading 412 files")
        #expect(p.fraction == nil)
    }

    @Test func indexingWithZeroTotalIsIndeterminate() {
        var s = session()
        let p = s.progress(library: library(scan("indexing", processed: 412, total: 0)))
        #expect(p.detail == "Reading 412 files")
        #expect(p.fraction == nil)
    }

    @Test func indexingClampsProcessedAboveTotal() {
        var s = session()
        let p = s.progress(library: library(scan("indexing", processed: 15, total: 10)))
        #expect(p.fraction == 1.0)
    }

    @Test func fractionNeverMovesBackwards() {
        var s = session()
        _ = s.progress(library: library(scan("indexing", processed: 6, total: 10)))
        let back = s.progress(library: library(scan("indexing", processed: 4, total: 10)))
        #expect(back.fraction == 0.6)
        let forward = s.progress(library: library(scan("indexing", processed: 8, total: 10)))
        #expect(forward.fraction == 0.8)
    }

    @Test func fractionResetsWhenStageChanges() {
        var s = session()
        _ = s.progress(library: library(scan("indexing", processed: 6, total: 10)))
        _ = s.progress(library: library(scan("discovering", discovered: 3)))
        let p = s.progress(library: library(scan("indexing", processed: 2, total: 10)))
        #expect(p.fraction == 0.2)
    }

    @Test func failureLineCountsFailuresAndSkipped() {
        var s = session()
        #expect(s.progress(library: library(scan("indexing", processed: 1, total: 10))).failureLine == nil)
        #expect(s.progress(library: library(scan("indexing", processed: 1, total: 10, failures: 1)))
            .failureLine == "1 file couldn't be read")
        #expect(s.progress(library: library(scan("indexing", processed: 1, total: 10, failures: 10, skipped: 2)))
            .failureLine == "12 files couldn't be read")
    }

    @Test func failureLineShowsOnDoneButNotOnOtherStages() {
        var s = session()
        let done = s.progress(library: library(scan("complete", failures: 2), tracks: 5))
        #expect(done.stage == .done)
        #expect(done.failureLine == "2 files couldn't be read")
        var t = session()
        #expect(t.progress(library: library(scan("discovering", failures: 2))).failureLine == nil)
        #expect(t.progress(library: library(scan("failed", failures: 2, error: "x"))).failureLine == nil)
    }

    @Test func completeWithTracksIsDone() {
        var s = session()
        let p = s.progress(library: library(scan("complete"), tracks: 1129,
                                            durationMs: 293_760_000, bytes: 41_700_000_000))
        #expect(p.stage == .done)
        #expect(p.title == "Your Music Is Ready")
        #expect(p.detail == DisplayFormat.totals(
            ListTotals(tracks: 1129, duration_ms: 293_760_000, size_bytes: 41_700_000_000)))
        #expect(p.fraction == nil)
    }

    @Test func completeWithNoTracksIsEmpty() {
        var s = session()
        let p = s.progress(library: library(scan("complete")))
        #expect(p.stage == .empty)
    }

    @Test func failedIsStopped() {
        var s = session()
        let p = s.progress(library: library(scan("failed", error: "disk vanished")))
        #expect(p.stage == .stopped)
        #expect(p.title == "Import Stopped")
        #expect(p.detail == "disk vanished")
    }

    @Test func interruptedWithoutErrorIsStopped() {
        var s = session()
        let p = s.progress(library: library(scan("interrupted")))
        #expect(p.stage == .stopped)
        #expect(p.detail == "The import did not finish.")
    }

    @Test func terminalBaselineReadsAsPreparing() {
        let stale = scan("failed", error: "old")
        var s = session(baseline: stale)
        #expect(s.progress(library: library(stale)).stage == .preparing)
    }

    @Test func finishedBetweenSnapshotsReachesDone() {
        var s = session(baseline: scan("idle", root: nil))
        let p = s.progress(library: library(scan("complete"), tracks: 3))
        #expect(p.stage == .done)
    }

    @Test func changedStatusClearsBaseline() {
        let stale = scan("failed", error: "old")
        var s = session(baseline: stale)
        _ = s.progress(library: library(scan("discovering")))
        #expect(s.progress(library: library(stale)).stage == .stopped)
    }

    @Test func retryRestoresBaseline() {
        let stale = scan("failed", error: "old")
        var s = session()
        #expect(s.progress(library: library(stale)).stage == .stopped)
        s.retry(baseline: stale)
        #expect(s.progress(library: library(stale)).stage == .preparing)
    }

    @Test func retryResetsTheShownFraction() {
        var s = session()
        _ = s.progress(library: library(scan("indexing", processed: 6, total: 10)))
        s.retry(baseline: scan("failed"))
        let p = s.progress(library: library(scan("indexing", processed: 2, total: 10)))
        #expect(p.fraction == 0.2)
    }

    @Test func firstFolderStartsSession() {
        let s = ImportSession.afterAddRoot(rootID: 7, rootsBefore: 0, current: nil, baseline: nil)
        #expect(s?.rootID == 7)
    }

    @Test func addingASecondFolderStartsNoSession() {
        #expect(ImportSession.afterAddRoot(rootID: 8, rootsBefore: 1, current: nil, baseline: nil) == nil)
    }

    private func sessionAt(_ stage: ImportProgress.Stage) -> ImportSession {
        var s = session()
        let library: LibraryState
        switch stage {
        case .empty: library = self.library(scan("complete"))
        case .done: library = self.library(scan("complete"), tracks: 5)
        case .stopped: library = self.library(scan("failed"))
        default: library = self.library(scan("indexing", processed: 1, total: 10))
        }
        _ = s.progress(library: library)
        return s
    }

    @Test func addAfterEmptyRestartsSession() {
        let s = ImportSession.afterAddRoot(rootID: 8, rootsBefore: 1, current: sessionAt(.empty), baseline: nil)
        #expect(s?.rootID == 8)
    }

    @Test func addWhileDoneStartsNoSession() {
        #expect(ImportSession.afterAddRoot(rootID: 8, rootsBefore: 1, current: sessionAt(.done), baseline: nil) == nil)
    }

    @Test func addWhileStoppedStartsNoSession() {
        #expect(ImportSession.afterAddRoot(rootID: 8, rootsBefore: 1, current: sessionAt(.stopped), baseline: nil) == nil)
    }

    @Test func addWhileImportingStartsNoSession() {
        #expect(ImportSession.afterAddRoot(rootID: 8, rootsBefore: 1, current: sessionAt(.indexing), baseline: nil) == nil)
    }

    @Test func cancelRetryBringsBackTheStoppedScreen() {
        var s = session()
        let failed = library(scan("failed", error: "gone"))
        let before = s.progress(library: failed)
        s.retry(baseline: failed.scan)
        #expect(s.progress(library: failed).stage == .preparing)
        s.cancelRetry()
        #expect(s.progress(library: failed) == before)
    }

    @Test func launchDuringFirstImportStartsSession() {
        let s = ImportSession.atLaunch(library: library(scan("indexing", processed: 1, total: 10)))
        #expect(s?.rootID == 7)
    }

    @Test func launchWithTracksStartsNoSession() {
        #expect(ImportSession.atLaunch(library: library(scan("indexing"), tracks: 5)) == nil)
        #expect(ImportSession.atLaunch(library: library(scan("idle"))) == nil)
        #expect(ImportSession.atLaunch(library: library(scan("indexing"), roots: 0)) == nil)
        #expect(ImportSession.atLaunch(library: library(scan("indexing", root: nil))) == nil)
    }

    private let note = ImportNote(socketPath: "/private/tmp/a/control.sock", rootID: 7)

    @Test func noteRestoresWhileTheRootIsScanning() {
        let lib = library(scan("indexing", processed: 3, total: 10), tracks: 40)
        #expect(ImportSession.atLaunch(library: lib, note: note, socketPath: "/private/tmp/a/control.sock")?.rootID == 7)
        #expect(ImportSession.atLaunch(library: library(scan("discovering"), tracks: 2), note: note,
                                       socketPath: "/private/tmp/a/control.sock")?.rootID == 7)
    }

    @Test func noteForAnotherSocketIsIgnored() {
        let lib = library(scan("indexing"), tracks: 40)
        #expect(ImportSession.atLaunch(library: lib, note: note, socketPath: "/private/tmp/b/control.sock") == nil)
    }

    @Test func noteForAnotherRootIsIgnored() {
        let lib = library(scan("indexing", root: 9), tracks: 40)
        #expect(ImportSession.atLaunch(library: lib, note: note, socketPath: "/private/tmp/a/control.sock") == nil)
    }

    @Test func noteIsIgnoredWhenTheScanIsNotActive() {
        for phase in ["complete", "idle", "failed", "interrupted"] {
            let lib = library(scan(phase), tracks: 40)
            #expect(ImportSession.atLaunch(library: lib, note: note, socketPath: "/private/tmp/a/control.sock") == nil)
        }
    }

    @Test func withoutANoteTheNoTracksRuleApplies() {
        #expect(ImportSession.atLaunch(library: library(scan("indexing")), note: nil, socketPath: "/private/tmp/a/control.sock")?.rootID == 7)
        #expect(ImportSession.atLaunch(library: library(scan("indexing"), tracks: 5), note: nil, socketPath: "/private/tmp/a/control.sock") == nil)
    }
}
