import Testing
@testable import TyrannyCore

@Suite struct ColumnVisibilityTests {
    private func shown(hidden: Bool, explicit: Bool, sorted: Bool) -> Bool {
        ColumnVisibility.isShown("date-added", hidden: hidden ? ["date-added"] : [],
                                 explicit: explicit ? ["date-added"] : [], sortedBy: sorted)
    }

    @Test func unhiddenColumnsAreShown() {
        #expect(shown(hidden: false, explicit: false, sorted: false))
        #expect(shown(hidden: false, explicit: true, sorted: false))
    }

    @Test func defaultHiddenColumnShowsOnlyWhileSortedBy() {
        #expect(!shown(hidden: true, explicit: false, sorted: false))
        #expect(shown(hidden: true, explicit: false, sorted: true))
    }

    @Test func anExplicitChoiceBeatsTheSort() {
        #expect(!shown(hidden: true, explicit: true, sorted: true))
        #expect(!shown(hidden: true, explicit: true, sorted: false))
    }
}
