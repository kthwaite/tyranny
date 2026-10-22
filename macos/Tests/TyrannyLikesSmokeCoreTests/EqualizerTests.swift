import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct EqualizerTests {
    @Test func defaultAndFactoryCurvesRoundTrip() throws {
        let settings = EqualizerSettings()
        #expect(!settings.enabled && settings.bands.isEmpty && settings.preamp_db == 0)
        #expect(settings.preset == .flat)
        for preset in EqualizerPreset.allCases {
            for enabled in [false, true] {
                let value = preset.settings(enabled: enabled)
                #expect(value.enabled == enabled && value.isValid && value.preset == preset)
                #expect(try JSONDecoder().decode(EqualizerSettings.self, from: JSONEncoder().encode(value)) == value)
                var custom = value
                custom.preamp_db += 0.5
                #expect(custom.preset == nil)
                if !value.bands.isEmpty {
                    custom = value
                    custom.bands[0].q += 0.1
                    #expect(custom.preset == nil)
                }
            }
        }
    }

    @Test func validationIncludesInactiveFieldsAndAllBoundaries() throws {
        for count in [0, 1, 16] {
            for preamp in [-96.0, 12] {
                #expect(EqualizerSettings(preamp_db: preamp, bands: Array(repeating: EqualizerBand(), count: count)).isValid)
            }
        }
        #expect(!EqualizerSettings(bands: Array(repeating: EqualizerBand(), count: 17)).isValid)
        for value in [-96.01, 12.01, .nan, .infinity, -.infinity] {
            #expect(!EqualizerSettings(preamp_db: value).isValid)
        }
        let fields: [(WritableKeyPath<EqualizerBand, Double>, [Double], [Double])] = [
            (\.frequency_hz, [20, 20000], [19.99, 20000.01]),
            (\.gain_db, [-24, 24], [-24.01, 24.01]),
            (\.q, [0.1, 18], [0.0999, 18.01]),
            (\.shelf_slope, [0.1, 1], [0.0999, 1.01])
        ]
        for kind in FilterKind.allCases {
            for (key, valid, invalid) in fields {
                for value in valid {
                    var band = EqualizerBand(enabled: false, kind: kind)
                    band[keyPath: key] = value
                    #expect(band.isValid)
                }
                for value in invalid + [.nan, .infinity, -.infinity] {
                    var band = EqualizerBand(enabled: false, kind: kind)
                    band[keyPath: key] = value
                    #expect(!band.isValid)
                }
            }
        }
        for slope in [UInt8(12), 24, 36, 48] { #expect(EqualizerBand(slope_db: slope).isValid) }
        for slope in [UInt8(0), 6, 18, 60, 255] { #expect(!EqualizerBand(slope_db: slope).isValid) }
        let invalid = EqualizerSettings(bands: [EqualizerBand(q: 19)])
        #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerSettings.self, from: JSONEncoder().encode(invalid)) }
    }

    @Test func wireRequiresEveryFieldAndRejectsUnknownFields() throws {
        let original = EqualizerSettings(enabled: true, preamp_db: -8, bands: [EqualizerBand(kind: .highPass, slope_db: 48)])
        let data = try JSONEncoder().encode(original)
        let object = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        for key in ["enabled", "preamp_db", "bands"] {
            var broken = object; broken.removeValue(forKey: key)
            let payload = try JSONSerialization.data(withJSONObject: broken)
            #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerSettings.self, from: payload) }
        }
        var unknown = object; unknown["obsolete"] = []
        let unknownData = try JSONSerialization.data(withJSONObject: unknown)
        #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerSettings.self, from: unknownData) }
        let band = try #require((object["bands"] as? [[String: Any]])?.first)
        for key in ["enabled", "kind", "frequency_hz", "gain_db", "q", "shelf_slope", "slope_db", "unknown"] {
            var broken = band
            if key == "unknown" { broken[key] = 1 } else { broken.removeValue(forKey: key) }
            let payload = try JSONSerialization.data(withJSONObject: broken)
            #expect(throws: DecodingError.self) { try JSONDecoder().decode(EqualizerBand.self, from: payload) }
        }
        let settings = PlayerSettings(gain_mode: "off", resume_on_start: false, volume: 100, repeat: .off,
                                      shuffle: false, equalizer: original,
                                      equalizer_presets: [.init(name: "Room", equalizer: original)])
        let settingsData = try JSONEncoder().encode(settings)
        #expect(try JSONDecoder().decode(PlayerSettings.self, from: settingsData) == settings)
        var missingPresets = try #require(JSONSerialization.jsonObject(with: settingsData) as? [String: Any])
        missingPresets.removeValue(forKey: "equalizer_presets")
        let missingData = try JSONSerialization.data(withJSONObject: missingPresets)
        #expect(throws: DecodingError.self) { try JSONDecoder().decode(PlayerSettings.self, from: missingData) }
    }

    @Test func bellShelfNotchResponseContract() {
        for frequency in [20.0, 1000, 16000, 20000] {
            for gain in [-24.0, -6, 6, 24] {
                for q in [0.1, 1, 18] {
                    let bell = EqualizerBand(frequency_hz: frequency, gain_db: gain, q: q)
                    #expect(abs(bell.responseDB(at: frequency) - gain) < 1e-7)
                }
                for kind in [FilterKind.lowShelf, .highShelf] {
                    for slope in [0.1, 0.5, 1] {
                        let band = EqualizerBand(kind: kind, frequency_hz: frequency, gain_db: gain, shelf_slope: slope)
                        #expect(abs(band.responseDB(at: frequency) - gain / 2) < 1e-7)
                        #expect(abs(band.responseDB(at: kind == .lowShelf ? 0 : 24000) - gain) < 1e-6)
                        #expect(abs(band.responseDB(at: kind == .lowShelf ? 24000 : 0)) < 1e-6)
                    }
                }
            }
            #expect(EqualizerBand(kind: .notch, frequency_hz: frequency).responseDB(at: frequency) < -150)
        }
    }

    @Test func butterworthOrdersAndResonanceUseIndependentAnalyticalResponse() {
        for kind in [FilterKind.lowPass, .highPass] {
            for slope in [UInt8(12), 24, 36, 48] {
                let band = EqualizerBand(kind: kind, frequency_hz: 1800, q: 1 / sqrt(2), slope_db: slope)
                for frequency in [20.0, 900, 1800, 3600, 16000, 20000] {
                    let ratio = tan(.pi * frequency / 48000) / tan(.pi * 1800 / 48000)
                    let factor = kind == .lowPass ? ratio : 1 / ratio
                    let expected = -10 * log10(1 + pow(factor, Double(slope) / 3))
                    #expect(abs(band.responseDB(at: frequency) - expected) < 1e-6)
                }
                // Only the highest-Q section is scaled; cutoff gain is independent of section count.
                for q in [0.1, 1, 18] {
                    var resonant = band; resonant.q = q
                    let expected = -10 * log10(2) + 20 * log10(q * sqrt(2))
                    #expect(abs(resonant.responseDB(at: 1800) - expected) < 1e-7)
                }
            }
        }
    }

    @Test func compositionPreampAndBypassPreview() {
        let a = EqualizerBand(frequency_hz: 1500, gain_db: 6)
        let b = EqualizerBand(kind: .highShelf, frequency_hz: 4000, gain_db: -4)
        let muted = EqualizerBand(enabled: false, gain_db: 24)
        let curve = EqualizerSettings(enabled: false, preamp_db: -3, bands: [a, b, muted])
        let frequencies = [20.0, 1000, 1500, 16000, 20000]
        let response = curve.responseDB(at: frequencies)
        for (i, f) in frequencies.enumerated() {
            #expect(abs(response[i] - (a.responseDB(at: f) + b.responseDB(at: f) - 3)) < 1e-10)
            #expect(response[i] == curve.responseDB(at: f))
            #expect(muted.responseDB(at: f) == 0)
        }
    }

    @Test func headroomRefinesNarrowOffGridPeaksAndReportsInsufficientRange() {
        let band = EqualizerBand(frequency_hz: 1731.213, gain_db: 24, q: 18)
        let narrow = EqualizerSettings(bands: [band])
        #expect(abs(narrow.peakGainDB - 24) < 1e-7)
        let combined = EqualizerSettings(bands: [band, EqualizerBand(frequency_hz: 1754.321, gain_db: 12, q: 18)])
        let referenceFrequencies = (0...32768).map { 1600 * pow(1900.0 / 1600, Double($0) / 32768) }
        let reference = combined.responseDB(at: referenceFrequencies).max() ?? 0
        #expect(combined.peakGainDB >= reference - 1e-7)
        #expect(abs(combined.peakGainDB - reference) < 0.001)
        let extreme = EqualizerSettings(bands: Array(repeating: band, count: 16))
        #expect(extreme.peakGainDB > 380)
        #expect(extreme.recommendedPreampDB == -96)
        #expect(EqualizerSettings().peakGainDB == 0)
    }

    @Test func graphicFactoryMigrationPreservesWarpedOctaveResponse() {
        let centers = [31.25, 62.5, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
        let presets: [(EqualizerPreset, Double, [Double])] = [
            (.bassBoost, -6, [5, 5, 3, 1, 0, 0, 0, 0, 0, 0]),
            (.trebleBoost, -6, [0, 0, 0, 0, 0, 0, 1, 3, 5, 5]),
            (.vocal, -4, [-2, -2, -1, 0, 1, 3, 3, 1, 0, -1])
        ]
        for (preset, preamp, gains) in presets {
            let curve = preset.settings(enabled: true)
            for f in [20.0, 100, 1000, 8000, 12000, 16000, 19000, 20000] {
                let w = 2 * Double.pi * f / 48000
                var expected = preamp
                for (center, gain) in zip(centers, gains) where gain != 0 {
                    let wc = 2 * Double.pi * center / 48000
                    let alpha = sin(wc) * sinh(log(2) / 2 * wc / sin(wc))
                    let a = pow(10, gain / 40)
                    let b = [1 + alpha * a, -2 * cos(wc), 1 - alpha * a]
                    let d = [1 + alpha / a, -2 * cos(wc), 1 - alpha / a]
                    func power(_ c: [Double]) -> Double {
                        pow(c[0] + c[1] * cos(w) + c[2] * cos(2 * w), 2)
                            + pow(c[1] * sin(w) + c[2] * sin(2 * w), 2)
                    }
                    expected += 10 * log10(power(b) / power(d))
                }
                #expect(abs(curve.responseDB(at: f) - expected) < 1e-8)
            }
        }
    }

    @Test func presetNamesMatchServerUnicodeScalarLimits() {
        for name in ["Room", "Café", String(repeating: "é", count: 80)] { #expect(EqualizerUserPreset.isValidName(name)) }
        for name in ["", " Room", "Room ", "A\nB", "A\u{007f}B", String(repeating: "e\u{301}", count: 41)] {
            #expect(!EqualizerUserPreset.isValidName(name))
        }
    }

    @Test func canonicallyEquivalentPresetNamesHaveDistinctServerIdentity() throws {
        let composed = EqualizerUserPreset(name: "\u{e9}", equalizer: EqualizerSettings())
        let decomposed = EqualizerUserPreset(name: "e\u{301}", equalizer: EqualizerSettings())
        #expect(composed.name == decomposed.name) // Swift's ordinary comparison is unsuitable here.
        #expect(composed.id != decomposed.id)
        #expect(composed != decomposed)
        #expect(composed.matchesName("\u{e9}"))
        #expect(!composed.matchesName("e\u{301}"))
        let presets = [composed, decomposed]
        #expect(Set(presets.map(\.id)).count == 2)
        #expect(presets.first { $0.id == decomposed.id }?.matchesName("e\u{301}") == true)
        #expect(try JSONDecoder().decode([EqualizerUserPreset].self, from: JSONEncoder().encode(presets)) == presets)
    }

    @Test func responsePlotIncludesNarrowNotchAndRefinedResonanceForBothCurves() throws {
        let notch = EqualizerBand(kind: .notch, frequency_hz: 19500, q: 18)
        let resonance = EqualizerBand(kind: .lowPass, frequency_hz: 19123, q: 18, slope_db: 48)
        let settings = EqualizerSettings(preamp_db: -4, bands: [notch, resonance])
        let plot = settings.responsePlot(selectedBand: 0)
        let selected = try #require(plot.selectedDB)
        let center = try #require(plot.frequencies.firstIndex(of: 19500))
        #expect(selected[center] < -150)
        #expect(plot.combinedDB[center] < -100)
        #expect(plot.frequencies.contains(19123))
        #expect(plot.frequencies == plot.frequencies.sorted())
        #expect(Set(plot.frequencies).count == plot.frequencies.count)

        let referenceFrequencies = (0...32768).map { 17000 + 3000 * Double($0) / 32768 }
        let reference = settings.responseDB(at: referenceFrequencies).max() ?? 0
        #expect(abs((plot.combinedDB.max() ?? 0) - reference) < 0.001)
        let selectedResonance = settings.responsePlot(selectedBand: 1)
        let selectedPeak = try #require(selectedResonance.selectedDB?.max())
        let resonanceReference = resonance.responseDB(at: referenceFrequencies).max() ?? 0
        #expect(abs(selectedPeak - resonanceReference) < 0.001)
        let resonancePeakIndex = try #require(selectedResonance.selectedDB?.firstIndex(of: selectedPeak))
        #expect(abs(selectedResonance.frequencies[resonancePeakIndex] - resonance.frequency_hz) > 0.01)
    }

    @Test func responsePlotRefinesOffCenterCombinedMinima() throws {
        let cut = EqualizerBand(frequency_hz: 1000, gain_db: -24, q: 18)
        let otherCut = EqualizerBand(frequency_hz: 1025, gain_db: -12, q: 18)
        let settings = EqualizerSettings(bands: [cut, otherCut])
        let plot = settings.responsePlot(selectedBand: 1)
        let minimum = try #require(plot.combinedDB.min())
        let index = try #require(plot.combinedDB.firstIndex(of: minimum))
        #expect(abs(plot.frequencies[index] - cut.frequency_hz) > 0.01)
        #expect(abs(plot.frequencies[index] - otherCut.frequency_hz) > 0.01)
        let frequencies = (0...32768).map { 950 + 125 * Double($0) / 32768 }
        let reference = try #require(settings.responseDB(at: frequencies).min())
        #expect(abs(minimum - reference) < 0.001)
    }
}
