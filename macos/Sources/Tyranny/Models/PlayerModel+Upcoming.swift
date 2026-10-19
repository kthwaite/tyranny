import Foundation
import TyrannyCore

extension PlayerModel {
    /// Whether the server would remove entries on `ReplaceUpcoming`: an entry after the current
    /// one, or, with nothing current, any unplayed entry (spec 5.1). The snapshot answers an
    /// empty queue or a lone current entry. Otherwise this reads the queue from its first entry,
    /// 500 at a time, until `UpNext.hasUpcoming` can tell. Played entries sit at the front (the
    /// server keeps at most 100), so one page usually does. Any failure answers true: a play
    /// then asks or keeps instead of replacing entries it could not see.
    func hasUpcoming() async -> Bool {
        guard let state = snapshot else { return false }
        if let answer = UpNext.quickAnswer(queueLength: state.queue_length, currentID: state.current_id) {
            return answer
        }
        var loaded: [QueueItem] = []
        var revision: UInt64?
        var restarted = false
        while true {
            do {
                let page: Page<QueueItem> = try await send("queue_page", [
                    "revision": revision.map(JSONValue.uint) ?? .null,
                    "offset": .int(loaded.count), "limit": .int(500)])
                revision = page.revision
                loaded += page.entries
                if let answer = UpNext.hasUpcoming(total: page.total, currentID: state.current_id, loaded: loaded) {
                    return answer
                }
                if page.entries.isEmpty { return true }
            } catch let failure as ServerFailure where failure.code == "stale_revision" && !restarted {
                restarted = true
                loaded = []
                revision = nil
            } catch {
                return true
            }
        }
    }
}
