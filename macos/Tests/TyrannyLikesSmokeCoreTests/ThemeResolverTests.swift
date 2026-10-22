import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct ThemeResolverTests {
    let art = RGB(hex: "#FF0000")!
    let custom = RGB(hex: "#3366FF")!
    let playingColour = RGB(hex: "#00AA00")!

    @Test func defaultsResolveToSystem() {
        #expect(ThemeResolver.resolve(settings: ColourSettings(), artColour: nil) == .system)
    }

    @Test func tintBeatsCustomAccent() {
        let settings = ColourSettings(custom: [.accent: custom], tintFromArtwork: true)
        let resolved = ThemeResolver.resolve(settings: settings, artColour: art)
        #expect(resolved.accent == .tinted(art))
        #expect(resolved.playing == .tinted(art))
    }

    @Test func tintOffUsesCustomAccent() {
        let settings = ColourSettings(custom: [.accent: custom], tintFromArtwork: false)
        let resolved = ThemeResolver.resolve(settings: settings, artColour: art)
        #expect(resolved.accent == .custom(custom))
        #expect(resolved.playing == .custom(custom))
    }

    @Test func noArtUsesCustomOrSystem() {
        let set = ColourSettings(custom: [.accent: custom], tintFromArtwork: true)
        #expect(ThemeResolver.resolve(settings: set, artColour: nil).accent == .custom(custom))
        #expect(ThemeResolver.resolve(settings: ColourSettings(), artColour: nil).accent == .system)
    }

    @Test func customPlayingIgnoresArt() {
        let settings = ColourSettings(custom: [.playing: playingColour], tintFromArtwork: true)
        let resolved = ThemeResolver.resolve(settings: settings, artColour: art)
        #expect(resolved.playing == .custom(playingColour))
        #expect(resolved.accent == .tinted(art))
    }

    @Test func otherTokensNeverTint() {
        let settings = ColourSettings(custom: [.error: custom], tintFromArtwork: true)
        let resolved = ThemeResolver.resolve(settings: settings, artColour: art)
        #expect(resolved.selection == .system)
        #expect(resolved.success == .system)
        #expect(resolved.warning == .system)
        #expect(resolved.error == .custom(custom))
    }
}
