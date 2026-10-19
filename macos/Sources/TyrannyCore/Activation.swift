import Foundation

/// What the user asked for on rows: Play (double-click, Return, Play), Play Next, or Add to Up Next
/// (⌥Return). Every list, grid and table decides through `ActivationPlan`.
public enum ActivationAction: Equatable, Sendable {
    case play
    case playNext
    case addToUpNext
}

public enum ActivationPlan: Equatable, Sendable {
    case enqueue(placement: Placement, play: Bool)
    /// Play with entries upcoming under the Ask policy: the "Clear Up Next?" sheet decides.
    case ask

    public static func make(_ action: ActivationAction, policy: UpNextPolicy, hasUpcoming: Bool) -> ActivationPlan {
        switch action {
        case .addToUpNext:
            return .enqueue(placement: .append, play: false)
        case .playNext:
            return .enqueue(placement: .next, play: false)
        case .play:
            switch SelectionEncoder.decide(policy: policy, hasUpcoming: hasUpcoming) {
            case .enqueue(let placement): return .enqueue(placement: placement, play: true)
            case .ask: return .ask
            }
        }
    }
}

/// The answer to "Clear Up Next?".
public enum UpNextChoice: Equatable, Sendable {
    case clear
    case keep

    public var placement: Placement { self == .clear ? .replaceUpcoming : .next }
    /// The policy "Don't ask again" stores.
    public var remembered: UpNextPolicy { self == .clear ? .replace : .keep }
}

/// Return on a row. Plain Return plays; ⌥Return adds to Up Next; any other modifier is ignored.
public enum ActivationKey {
    public static func action(option: Bool, otherModifiers: Bool) -> ActivationAction? {
        if otherModifiers { return nil }
        return option ? .addToUpNext : .play
    }
}

/// One `enqueue_tracks` request.
public struct EnqueueRequest: Equatable, Sendable {
    public let selection: TrackSelection
    public let placement: Placement
    public let play: Bool
    public let source: QueueSource

    public init(selection: TrackSelection, placement: Placement, play: Bool, source: QueueSource) {
        self.selection = selection
        self.placement = placement
        self.play = play
        self.source = source
    }

    public func fields(expectedRevision: UInt64) throws -> [String: JSONValue] {
        ["selection": try JSONValue.encoding(selection), "expected_revision": .uint(expectedRevision),
         "placement": .string(placement.rawValue), "play": .bool(play), "source": try JSONValue.encoding(source)]
    }
}

public enum RunSource {
    /// The queue source a list stamps on the entries it enqueues. `name` is the list's own name.
    public static func queueSource(list: ListSource, name: String) -> QueueSource {
        let kind: SourceKind
        switch list {
        case .library: kind = .songs
        case .album: kind = .album
        case .artist, .appearsOn: kind = .artist
        case .compilations: kind = .compilations
        case .genre: kind = .genre
        case .recent: kind = .recent
        case .playlist: kind = .playlist
        case .smart: kind = .smart
        }
        return QueueSource(kind: kind, name: name)
    }
}
