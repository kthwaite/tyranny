import Foundation

public struct EqualizerSettings: Codable, Sendable, Equatable {
    public static let frequencies: [Double] = [31.25, 62.5, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
    public static let labels = ["31.25 Hz", "62.5 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz"]
    public var enabled: Bool
    public var preamp_db: Double
    public var bands_db: [Double]

    public init(enabled: Bool = false, preamp_db: Double = 0, bands_db: [Double] = Array(repeating: 0, count: 10)) {
        self.enabled = enabled
        self.preamp_db = preamp_db
        self.bands_db = bands_db
    }

    public var isValid: Bool {
        preamp_db.isFinite && (-24...12).contains(preamp_db)
            && bands_db.count == Self.frequencies.count
            && bands_db.allSatisfy { $0.isFinite && (-12...12).contains($0) }
    }

    private enum CodingKeys: String, CodingKey { case enabled, preamp_db, bands_db }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try values.decode(Bool.self, forKey: .enabled)
        preamp_db = try values.decode(Double.self, forKey: .preamp_db)
        bands_db = try values.decode([Double].self, forKey: .bands_db)
        guard isValid else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "Equalizer requires ten finite gains in −12…12 dB and a finite preamp in −24…12 dB."))
        }
    }

    /// Bypass preserves the curve and does not change its preset name.
    public var preset: EqualizerPreset? {
        EqualizerPreset.allCases.first {
            let candidate = $0.settings(enabled: enabled)
            return candidate == self
        }
    }
}

public enum EqualizerPreset: String, CaseIterable, Sendable {
    case flat = "Flat"
    case bassBoost = "Bass Boost"
    case trebleBoost = "Treble Boost"
    case vocal = "Vocal"

    /// Presets replace the entire curve, while preserving the user's bypass choice.
    public func settings(enabled: Bool) -> EqualizerSettings {
        switch self {
        case .flat:
            EqualizerSettings(enabled: enabled)
        case .bassBoost:
            EqualizerSettings(enabled: enabled, preamp_db: -6, bands_db: [5, 5, 3, 1, 0, 0, 0, 0, 0, 0])
        case .trebleBoost:
            EqualizerSettings(enabled: enabled, preamp_db: -6, bands_db: [0, 0, 0, 0, 0, 0, 1, 3, 5, 5])
        case .vocal:
            EqualizerSettings(enabled: enabled, preamp_db: -4, bands_db: [-2, -2, -1, 0, 1, 3, 3, 1, 0, -1])
        }
    }
}
