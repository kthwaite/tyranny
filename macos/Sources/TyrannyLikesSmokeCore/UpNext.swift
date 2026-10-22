import Foundation

public enum UpNext {
    /// Whether the queue has an entry the server treats as upcoming (what `ClearUpcoming` and
    /// `ReplaceUpcoming` remove). The server counts them with the shuffle-aware rule
    /// (`UpNextSections`) in every snapshot, so the snapshot answers at its queue revision.
    public static func hasUpcoming(_ state: ControlState) -> Bool {
        state.upcoming_count > 0
    }
}
