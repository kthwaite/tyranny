import Testing
@testable import TyrannyCore

@Suite struct NowPlayingArtTests {
    @Test func collapsedShowsInTheBar() {
        #expect(NowPlayingArt.placement(expanded: false, sidebarVisible: true) == .bar)
    }

    @Test func expandedShowsInTheSidebar() {
        #expect(NowPlayingArt.placement(expanded: true, sidebarVisible: true) == .sidebar)
    }

    @Test func expandedFallsBackToTheBarWhileTheSidebarIsHidden() {
        #expect(NowPlayingArt.placement(expanded: true, sidebarVisible: false) == .bar)
    }
}
