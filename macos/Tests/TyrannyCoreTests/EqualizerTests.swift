import Foundation
import Testing
@testable import TyrannyCore

@Suite struct EqualizerTests {
    @Test func defaultIsBypassedFlat() {
        let settings = EqualizerSettings()
        #expect(!settings.enabled)
        #expect(settings.preamp_db == 0)
        #expect(settings.bands_db == Array(repeating: 0, count: 10))
        #expect(settings.preset == .flat)
        #expect(settings.isValid)
        #expect(EqualizerSettings.frequencies == [31.25, 62.5, 125, 250, 500, 1000, 2000, 4000, 8000, 16000])
    }

    @Test func presetsReplaceFullCurvesAndPreserveBypass() throws {
        for preset in EqualizerPreset.allCases {
            for enabled in [true, false] {
                let settings = preset.settings(enabled: enabled)
                #expect(settings.enabled == enabled)
                #expect(settings.isValid)
                #expect(settings.preset == preset)
                #expect(try JSONDecoder().decode(EqualizerSettings.self, from: JSONEncoder().encode(settings)) == settings)
                if preset != .flat { #expect(settings.preamp_db < 0) }
            }
        }
    }

    @Test func customIsDerivedFromEveryGainNotStoredName() {
        for preset in EqualizerPreset.allCases {
            var settings = preset.settings(enabled: true)
            settings.preamp_db += 0.5
            #expect(settings.preset == nil)
            settings = preset.settings(enabled: false)
            for band in settings.bands_db.indices {
                var custom = settings
                custom.bands_db[band] += 0.5
                #expect(custom.preset == nil)
            }
        }
    }

    @Test func settingsWireRequiresEqualizer() throws {
        let legacy = Data(#"{"gain_mode":"off","resume_on_start":false,"volume":100,"repeat":"off","shuffle":false}"#.utf8)
        #expect(throws: DecodingError.self) { try JSONDecoder().decode(PlayerSettings.self, from: legacy) }
        let settings = PlayerSettings(gain_mode: "off", resume_on_start: false, volume: 100, repeat: .off,
                                      shuffle: false, equalizer: EqualizerPreset.vocal.settings(enabled: true))
        #expect(try JSONDecoder().decode(PlayerSettings.self, from: JSONEncoder().encode(settings)) == settings)
        let encoded = try JSONValue.encoding(settings.equalizer)
        #expect(encoded == .object(["enabled": .bool(true), "preamp_db": .int(-4),
                                    "bands_db": .array([-2, -2, -1, 0, 1, 3, 3, 1, 0, -1].map { $0 < 0 ? .int($0) : .uint(UInt64($0)) })]))
    }

    @Test func validationMatchesServerBounds() throws {
        for preamp in [-24.0, 12.0] {
            for band in [-12.0, 12.0] {
                #expect(EqualizerSettings(preamp_db: preamp, bands_db: Array(repeating: band, count: 10)).isValid)
            }
        }
        for value in [-24.5, 12.5, .infinity, -.infinity, .nan] {
            #expect(!EqualizerSettings(preamp_db: value).isValid)
        }
        for value in [-12.5, 12.5, .infinity, -.infinity, .nan] {
            #expect(!EqualizerSettings(bands_db: Array(repeating: value, count: 10)).isValid)
        }
        for count in [0, 9, 11] {
            let invalid = EqualizerSettings(bands_db: Array(repeating: 0, count: count))
            #expect(!invalid.isValid)
            let data = try JSONEncoder().encode(invalid)
            #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerSettings.self, from: data) }
        }
        for invalid in [EqualizerSettings(preamp_db: 13), EqualizerSettings(bands_db: Array(repeating: -13, count: 10))] {
            let data = try JSONEncoder().encode(invalid)
            #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerSettings.self, from: data) }
        }
    }
}
