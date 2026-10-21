import Foundation
import Testing
@testable import TyrannyCore

@Suite struct ColourSettingsTests {
    private func withFreshDefaults(_ body: (UserDefaults) -> Void) {
        let name = "dev.tyranny.test.colours.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        body(defaults)
    }

    @Test func hexRoundTripsAndNormalisesCase() {
        #expect(RGB(hex: "#3366ff")?.hex == "#3366FF")
        #expect(RGB(hex: "#000000") == RGB(red: 0, green: 0, blue: 0))
        #expect(RGB(red: 0.5, green: 0.5, blue: 0.5).hex == "#808080")
        for h in ["#000000", "#FFFFFF", "#3366FF", "#ECECEC", "#1E1E1E"] {
            #expect(RGB(hex: h)?.hex == h)
        }
    }

    @Test(arguments: ["", "#", "3366FF", "#3366F", "#3366FFF", "#3366FF00", "#GG0000",
                      " #3366FF", "#3366FF ", "#+12345", "#-12345", "#٣٣٦٦FF", "#３３66FF"])
    func hexRejectsAnythingButHashAndSixHexDigits(_ text: String) {
        #expect(RGB(hex: text) == nil)
    }

    @Test func componentsClampToUnitRange() {
        #expect(RGB(red: 1.5, green: -0.2, blue: .nan) == RGB(red: 1, green: 0, blue: 0))
    }

    @Test func defaultsAreSystemWithTintOn() {
        #expect(ColourSettings().custom.isEmpty)
        #expect(ColourSettings().tintFromArtwork)
        withFreshDefaults { fresh in
            #expect(ColourSettings(defaults: fresh) == ColourSettings())
        }
    }

    @Test func saveAndLoadRoundTrip() {
        withFreshDefaults { defaults in
            var settings = ColourSettings()
            settings[.accent] = RGB(hex: "#3366FF")
            settings[.selection] = RGB(hex: "#FFE066")
            settings.tintFromArtwork = false
            settings.save(to: defaults)
            #expect(ColourSettings(defaults: defaults) == settings)
            #expect(defaults.string(forKey: "colour.accent") == "#3366FF")
            #expect(defaults.object(forKey: "colour.playing") == nil)
            #expect(defaults.object(forKey: "colour.tintFromArtwork") as? Bool == false)
        }
    }

    @Test func savingSystemRemovesTheKey() {
        withFreshDefaults { defaults in
            var settings = ColourSettings()
            settings[.accent] = RGB(hex: "#3366FF")
            settings.save(to: defaults)
            settings[.accent] = nil
            settings.save(to: defaults)
            #expect(defaults.object(forKey: "colour.accent") == nil)
        }
    }

    @Test func malformedStoredValuesReadAsSystem() {
        withFreshDefaults { defaults in
            defaults.set("blue", forKey: "colour.accent")
            defaults.set(42, forKey: "colour.error")
            defaults.set("yes", forKey: "colour.tintFromArtwork")
            #expect(ColourSettings(defaults: defaults) == ColourSettings())
        }
    }

    @Test func applySetsAndClearsAToken() throws {
        var settings = ColourSettings()
        try settings.apply(CommandLanguage.parse("colour error #AA0000"))
        #expect(settings[.error] == RGB(hex: "#AA0000"))
        try settings.apply(CommandLanguage.parse("colour error system"))
        #expect(settings[.error] == nil)
    }

    @Test func coloursResetRestoresDefaults() throws {
        var settings = ColourSettings(custom: [.accent: RGB(hex: "#3366FF")!], tintFromArtwork: false)
        try settings.apply(CommandLanguage.parse("colours reset"))
        #expect(settings == ColourSettings())
    }

    @Test func tintCommandSetsTheToggle() throws {
        var settings = ColourSettings()
        try settings.apply(CommandLanguage.parse("tint off"))
        #expect(!settings.tintFromArtwork)
        try settings.apply(CommandLanguage.parse("tint on"))
        #expect(settings.tintFromArtwork)
    }

    @Test func applyRejectsOtherCommands() throws {
        var settings = ColourSettings()
        let play = try CommandLanguage.parse("play")
        #expect(throws: CommandLanguageError.self) { try settings.apply(play) }
    }
}
