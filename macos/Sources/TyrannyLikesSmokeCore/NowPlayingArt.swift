/// Where the now-playing art shows: the player bar's thumbnail, or the large square at the
/// foot of the sidebar.
public enum NowPlayingArt {
    public enum Placement: Equatable, Sendable { case bar, sidebar }

    /// Expanded art shows in the sidebar only while the sidebar is visible.
    public static func placement(expanded: Bool, sidebarVisible: Bool) -> Placement {
        expanded && sidebarVisible ? .sidebar : .bar
    }
}
