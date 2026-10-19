import Foundation
import TyrannyCore

extension PlayerModel {
    /// Whether the server would remove entries on `ReplaceUpcoming`: an entry after the current
    /// one, or, with nothing current, any unplayed entry (spec 5.1). The snapshot answers an
    /// empty queue or a lone current entry. Otherwise this reads the queue from its first entry
    /// at `revision`, 500 at a time, and stops as soon as `UpNext.Scan` can tell. Played entries
    /// sit at the front (the server keeps at most 100), so one page usually does. Entries are
    /// counted, not kept. A `stale_revision` reply throws: the caller refetches and decides
    /// again. Any other failure answers true, so a play asks or keeps instead of replacing
    /// entries it could not see.
    func hasUpcoming(atQueueRevision revision: UInt64) async throws -> Bool {
        guard let state = snapshot else { return false }
        if let answer = UpNext.quickAnswer(queueLength: state.queue_length, currentID: state.current_id) {
            return answer
        }
        var scan = UpNext.Scan(currentID: state.current_id)
        var offset = 0
        while true {
            do {
                let page: Page<QueueItem> = try await send("queue_page", [
                    "revision": .uint(revision), "offset": .int(offset), "limit": .int(500)])
                if let answer = scan.feed(page.entries, total: page.total) { return answer }
                if page.entries.isEmpty { return true }
                offset += page.entries.count
            } catch let failure as ServerFailure where failure.code == "stale_revision" {
                throw failure
            } catch {
                return true
            }
        }
    }
}
