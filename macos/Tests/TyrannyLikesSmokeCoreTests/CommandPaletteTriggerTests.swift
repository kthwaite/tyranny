import Testing
import TyrannyLikesSmokeCore

struct CommandPaletteTriggerTests {
    private struct Case {
        let characters: String?
        let hasCommandModifiers: Bool
        let isPaletteWindow: Bool
        let hasSheet: Bool
        let isEditingText: Bool
        let opens: Bool
    }

    @Test func triggerIgnoresTextEditingSheetsAndOtherWindows() {
        let cases = [
            Case(characters: ":", hasCommandModifiers: false, isPaletteWindow: true, hasSheet: false, isEditingText: false, opens: true),
            Case(characters: ";", hasCommandModifiers: false, isPaletteWindow: true, hasSheet: false, isEditingText: false, opens: false),
            Case(characters: ":", hasCommandModifiers: true, isPaletteWindow: true, hasSheet: false, isEditingText: false, opens: false),
            Case(characters: ":", hasCommandModifiers: false, isPaletteWindow: false, hasSheet: false, isEditingText: false, opens: false),
            Case(characters: ":", hasCommandModifiers: false, isPaletteWindow: true, hasSheet: true, isEditingText: false, opens: false),
            Case(characters: ":", hasCommandModifiers: false, isPaletteWindow: true, hasSheet: false, isEditingText: true, opens: false),
            Case(characters: nil, hasCommandModifiers: false, isPaletteWindow: true, hasSheet: false, isEditingText: false, opens: false),
        ]
        for item in cases {
            let opens = CommandPaletteTrigger.shouldOpen(
                characters: item.characters, hasCommandModifiers: item.hasCommandModifiers,
                isPaletteWindow: item.isPaletteWindow, hasSheet: item.hasSheet, isEditingText: item.isEditingText)
            #expect(opens == item.opens, "\(item)")
        }
    }
}
