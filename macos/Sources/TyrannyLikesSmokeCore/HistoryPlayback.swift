import Foundation

public enum HistoryPlayback {
    /// The source label of History activations. The server has no history source kind;
    /// `songs` repeats over the queue scope, which is right for a single track.
    public static let source = QueueSource(kind: .songs, name: "History")

    public static func selection(trackID: UInt64, libraryRevision: UInt64) -> TrackSelection {
        .ids(libraryRevision: libraryRevision, trackIDs: [trackID])
    }
}
