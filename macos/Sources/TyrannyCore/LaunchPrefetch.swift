import Foundation

/// The launch prefetch's ok reply, with the server state it was answered in.
public struct LaunchPrefetchReply: Sendable, Equatable {
    /// The reply's `result` object: the bytes a normal request returns.
    public let body: Data
    /// The prefetch connection's hello `instance_id`.
    public let instanceID: String
    /// The latest `history_revision` the prefetch connection saw before the reply: the hello
    /// snapshot's, or a later `state` message's.
    public let historyRevision: UInt64

    public init(body: Data, instanceID: String, historyRevision: UInt64) {
        self.body = body
        self.instanceID = instanceID
        self.historyRevision = historyRevision
    }

    /// Whether the reply may stand in for a request on the main connection now. The rows must
    /// come from the same server instance, at the library revision of the main connection's
    /// snapshot, with no history change since (play counts move without a library revision).
    /// `pageRevision` is the reply's own `revision`. Nil current values never match.
    public func isFresh(pageRevision: UInt64, instanceID current: String?, libraryRevision: UInt64?,
                        historyRevision currentHistory: UInt64?) -> Bool {
        instanceID == current && pageRevision == libraryRevision && historyRevision == currentHistory
    }
}

/// Holds the reply of the one list request the client sends early at launch.
/// The first fetch whose request equals the prefetch's takes the reply; every other fetch
/// (or any later one) gets nil and sends its own request.
public actor LaunchPrefetchSlot {
    private enum State {
        case pending
        case waiting(CheckedContinuation<LaunchPrefetchReply?, Never>)
        case ready(LaunchPrefetchReply?)
        case claimed
    }

    private let request: [String: JSONValue]
    private var state = State.pending
    private var timer: Task<Void, Never>?

    /// `request` is the prefetch's request fields (everything but `command` and `id`).
    public init(request: [String: JSONValue]) {
        self.request = request
    }

    /// The prefetch finished: its ok reply, or nil for any failure. Later calls are ignored.
    public func fulfil(_ reply: LaunchPrefetchReply?) {
        switch state {
        case .pending:
            state = .ready(reply)
        case .waiting(let continuation):
            state = .claimed
            continuation.resume(returning: reply)
        case .ready, .claimed:
            break
        }
    }

    /// One-shot. Returns the reply when this is the first claim, `request` equals the prefetch's
    /// request, and the prefetch succeeds by `deadline`; otherwise nil. The slot is empty afterwards.
    public func claim(_ request: [String: JSONValue],
                      until deadline: ContinuousClock.Instant) async -> LaunchPrefetchReply? {
        let held = state
        switch held {
        case .claimed, .waiting:
            return nil
        case .ready(let reply):
            state = .claimed
            return request == self.request ? reply : nil
        case .pending:
            guard request == self.request else {
                state = .claimed
                return nil
            }
            let reply = await withCheckedContinuation { (continuation: CheckedContinuation<LaunchPrefetchReply?, Never>) in
                state = .waiting(continuation)
                timer = Task { [weak self] in
                    try? await Task.sleep(until: deadline, clock: .continuous)
                    await self?.expire()
                }
            }
            timer?.cancel()
            timer = nil
            return reply
        }
    }

    /// true until the prefetch finishes or a claim takes the slot.
    public var isPending: Bool {
        if case .pending = state { return true }
        return false
    }

    /// The deadline passed while a claim waits: give up. A no-op once `fulfil` already resumed it.
    private func expire() {
        guard case .waiting(let continuation) = state else { return }
        state = .claimed
        continuation.resume(returning: nil)
    }
}

/// A track list's `list_tracks` request. `ListModel` and the launch prefetch both build theirs
/// here, so equal inputs give structurally equal fields.
public enum TrackListRequest {
    /// Rows per window. `ListWindowCache` uses it by default, so it is a list's first `limit`.
    public static let windowSize = 200

    /// The description a track list shows. `browser` is nil while the column browser is hidden or
    /// does not narrow this list. With no sort picked, the destination's default sort applies.
    public static func description(source: ListSource, browser: Browser?, query: String, sort: [SortKey],
                                   defaultSort: [SortKey], group: Grouping) -> ListDescription {
        ListDescription(source: source, browser: browser ?? Browser(), query: query,
                        sort: sort.isEmpty ? defaultSort : sort, group: group)
    }

    /// The fields of one window's request: everything but `command` and `id`.
    public static func fields(list: ListDescription, revision: UInt64?, offset: Int,
                              limit: Int) throws -> [String: JSONValue] {
        ["list": try JSONValue.encoding(list), "revision": revision.map(JSONValue.uint) ?? .null,
         "offset": .int(offset), "limit": .int(limit)]
    }

    /// The Songs list's first request at launch. A new list has no browser selection and no picked
    /// sort, and the filter field starts empty. Its first window is unpinned.
    public static func launchSongs(defaultSort: [SortKey], group: Grouping) throws -> [String: JSONValue] {
        try fields(list: description(source: .library, browser: nil, query: "", sort: [],
                                     defaultSort: defaultSort, group: group),
                   revision: nil, offset: 0, limit: windowSize)
    }
}
