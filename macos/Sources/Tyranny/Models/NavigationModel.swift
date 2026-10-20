import Foundation
import Observation
import TyrannyCore

enum SettingsTab: String, Hashable {
    case connection, library, playback
}

/// Window navigation state: the source-list destination, the filter text,
/// and the client-local view preferences.
@MainActor @Observable
final class NavigationModel {
    var destination: Destination = .songs
    var filter = ""
    /// Set by the current view's list when the server rejects `filter`.
    var filterError: String?
    var settingsTab: SettingsTab = .connection
    /// Incremented by ⌘F; the filter field takes focus when it changes.
    private(set) var filterFocusRequest = 0

    /// An album or artist to show and select: Show in Album and Show in Artist set it, and the
    /// window moves to the destination and clears it.
    enum RevealTarget: Equatable {
        case album(UInt64)
        case artist(UInt64)
    }
    var reveal: RevealTarget?
    /// Incremented by ⌘L; the shown track list reveals the playing row when it changes.
    private(set) var revealPlayingRequest = 0

    var columnBrowserVisible: Bool {
        didSet { defaults.set(columnBrowserVisible, forKey: "columnBrowserVisible") }
    }
    var upNextDrawerVisible: Bool {
        didSet { defaults.set(upNextDrawerVisible, forKey: "upNextDrawerVisible") }
    }
    /// The player bar's art is expanded into the foot of the sidebar.
    var nowPlayingArtExpanded: Bool {
        didSet { defaults.set(nowPlayingArtExpanded, forKey: "nowPlayingArtExpanded") }
    }

    @ObservationIgnored private let defaults: UserDefaults
    /// Makes `grouping(for:)` observable; the values live in `defaults`.
    private var groupingRevision = 0

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        columnBrowserVisible = defaults.bool(forKey: "columnBrowserVisible")
        upNextDrawerVisible = defaults.bool(forKey: "upNextDrawerVisible")
        nowPlayingArtExpanded = defaults.bool(forKey: "nowPlayingArtExpanded")
    }

    func grouping(for destination: Destination) -> Grouping {
        _ = groupingRevision
        return defaults.string(forKey: Self.groupingKey(destination)).flatMap(Grouping.init(rawValue:))
            ?? destination.defaultGrouping
    }

    func setGrouping(_ grouping: Grouping, for destination: Destination) {
        defaults.set(grouping.rawValue, forKey: Self.groupingKey(destination))
        groupingRevision &+= 1
    }

    func focusFilter() { filterFocusRequest &+= 1 }
    func revealPlaying() { revealPlayingRequest &+= 1 }

    static func groupingKey(_ destination: Destination) -> String { "group.\(destination.key)" }
}
