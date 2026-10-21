import Foundation

public enum FilterKind: String, Codable, CaseIterable, Sendable {
    case bell, lowShelf = "low_shelf", highShelf = "high_shelf"
    case highPass = "high_pass", lowPass = "low_pass", notch

    public var label: String {
        switch self {
        case .bell: "Bell"
        case .lowShelf: "Low Shelf"
        case .highShelf: "High Shelf"
        case .highPass: "High Pass"
        case .lowPass: "Low Pass"
        case .notch: "Notch"
        }
    }
    public var usesGain: Bool { self == .bell || isShelf }
    public var isShelf: Bool { self == .lowShelf || self == .highShelf }
    public var isPass: Bool { self == .lowPass || self == .highPass }
}

// Decode every field and reject unknown fields rather than accepting old wire configurations.
private struct EqualizerKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}
private func requireKeys(_ decoder: any Decoder, _ allowed: Set<String>) throws {
    let keys = try decoder.container(keyedBy: EqualizerKey.self).allKeys
    if keys.contains(where: { !allowed.contains($0.stringValue) }) {
        throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Unknown equalizer field"))
    }
}

public struct EqualizerBand: Codable, Sendable, Equatable {
    public var enabled: Bool
    public var kind: FilterKind
    public var frequency_hz: Double
    public var gain_db: Double
    public var q: Double
    public var shelf_slope: Double
    public var slope_db: UInt8

    public init(enabled: Bool = true, kind: FilterKind = .bell, frequency_hz: Double = 1000,
                gain_db: Double = 0, q: Double = 1, shelf_slope: Double = 1, slope_db: UInt8 = 12) {
        self.enabled = enabled
        self.kind = kind
        self.frequency_hz = frequency_hz
        self.gain_db = gain_db
        self.q = q
        self.shelf_slope = shelf_slope
        self.slope_db = slope_db
    }
    public var isValid: Bool {
        frequency_hz.isFinite && (20...20000).contains(frequency_hz)
            && gain_db.isFinite && (-24...24).contains(gain_db)
            && q.isFinite && (0.1...18).contains(q)
            && shelf_slope.isFinite && (0.1...1).contains(shelf_slope)
            && [12, 24, 36, 48].contains(slope_db)
    }
    private enum CodingKeys: String, CodingKey, CaseIterable {
        case enabled, kind, frequency_hz, gain_db, q, shelf_slope, slope_db
    }
    public init(from decoder: any Decoder) throws {
        try requireKeys(decoder, Set(CodingKeys.allCases.map(\.rawValue)))
        let v = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try v.decode(Bool.self, forKey: .enabled)
        kind = try v.decode(FilterKind.self, forKey: .kind)
        frequency_hz = try v.decode(Double.self, forKey: .frequency_hz)
        gain_db = try v.decode(Double.self, forKey: .gain_db)
        q = try v.decode(Double.self, forKey: .q)
        shelf_slope = try v.decode(Double.self, forKey: .shelf_slope)
        slope_db = try v.decode(UInt8.self, forKey: .slope_db)
        guard isValid else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid parametric equalizer band"))
        }
    }

    /// Response at the fixed 48 kHz engine rate. Bypassed bands have exact unity response.
    public func responseDB(at frequency: Double) -> Double {
        guard enabled else { return 0 }
        return sections.reduce(0) { $0 + $1.responseDB(at: frequency) }
    }

    public func responseDB(at frequencies: [Double]) -> [Double] {
        guard enabled else { return Array(repeating: 0, count: frequencies.count) }
        let coefficients = sections
        return frequencies.map { frequency in
            coefficients.reduce(0) { $0 + $1.responseDB(at: frequency) }
        }
    }

    fileprivate var sections: [EqualizerSection] {
        if kind.usesGain && gain_db == 0 { return [] }
        let w = 2 * Double.pi * frequency_hz / 48000
        let c = cos(w), s = sin(w), a = pow(10, gain_db / 40)
        let alpha = s / (2 * q)
        switch kind {
        case .bell:
            return [.init(1 + alpha * a, -2 * c, 1 - alpha * a,
                          1 + alpha / a, -2 * c, 1 - alpha / a)]
        case .notch:
            return [.init(1, -2 * c, 1, 1 + alpha, -2 * c, 1 - alpha)]
        case .lowShelf, .highShelf:
            let shelfAlpha = s / 2 * sqrt((a + 1 / a) * (1 / shelf_slope - 1) + 2)
            let beta = 2 * sqrt(a) * shelfAlpha
            if kind == .lowShelf {
                return [.init(a * ((a + 1) - (a - 1) * c + beta), 2 * a * ((a - 1) - (a + 1) * c),
                              a * ((a + 1) - (a - 1) * c - beta),
                              (a + 1) + (a - 1) * c + beta, -2 * ((a - 1) + (a + 1) * c),
                              (a + 1) + (a - 1) * c - beta)]
            }
            return [.init(a * ((a + 1) + (a - 1) * c + beta), -2 * a * ((a - 1) + (a + 1) * c),
                          a * ((a + 1) + (a - 1) * c - beta),
                          (a + 1) - (a - 1) * c + beta, 2 * ((a - 1) - (a + 1) * c),
                          (a + 1) - (a - 1) * c - beta)]
        case .highPass, .lowPass:
            let order = Int(slope_db) / 6
            return (0..<(order / 2)).map { k in
                let resonance = k == order / 2 - 1 ? q * sqrt(2) : 1
                let sectionQ = resonance / (2 * cos(Double(2 * k + 1) * .pi / Double(2 * order)))
                let sectionAlpha = s / (2 * sectionQ)
                let sign = kind == .lowPass ? 1.0 : -1.0
                let b = (1 - sign * c) / 2
                return EqualizerSection(b, sign * 2 * b, b, 1 + sectionAlpha, -2 * c, 1 - sectionAlpha)
            }
        }
    }
}

fileprivate struct EqualizerSection {
    let b0: Double, b1: Double, b2: Double, a0: Double, a1: Double, a2: Double
    init(_ b0: Double, _ b1: Double, _ b2: Double, _ a0: Double, _ a1: Double, _ a2: Double) {
        self.b0 = b0; self.b1 = b1; self.b2 = b2
        self.a0 = a0; self.a1 = a1; self.a2 = a2
    }
    func responseDB(at frequency: Double) -> Double {
        let w = 2 * Double.pi * frequency / 48000
        let c = cos(w), s = sin(w), c2 = cos(2 * w), s2 = sin(2 * w)
        let nr = b0 + b1 * c + b2 * c2, ni = b1 * s + b2 * s2
        let dr = a0 + a1 * c + a2 * c2, di = a1 * s + a2 * s2
        return 10 * log10(max(1e-30, (nr * nr + ni * ni) / (dr * dr + di * di)))
    }
}

public struct EqualizerSettings: Codable, Sendable, Equatable {
    public var enabled: Bool
    public var preamp_db: Double
    public var bands: [EqualizerBand]

    public init(enabled: Bool = false, preamp_db: Double = 0, bands: [EqualizerBand] = []) {
        self.enabled = enabled; self.preamp_db = preamp_db; self.bands = bands
    }
    public var isValid: Bool {
        preamp_db.isFinite && (-96...12).contains(preamp_db)
            && bands.count <= 16 && bands.allSatisfy(\.isValid)
    }
    private enum CodingKeys: String, CodingKey, CaseIterable { case enabled, preamp_db, bands }
    public init(from decoder: any Decoder) throws {
        try requireKeys(decoder, Set(CodingKeys.allCases.map(\.rawValue)))
        let v = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try v.decode(Bool.self, forKey: .enabled)
        preamp_db = try v.decode(Double.self, forKey: .preamp_db)
        bands = try v.decode([EqualizerBand].self, forKey: .bands)
        guard isValid else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Equalizer requires 0–16 valid bands and preamp −96…12 dB"))
        }
    }
    public var preset: EqualizerPreset? {
        EqualizerPreset.allCases.first { $0.settings(enabled: enabled) == self }
    }
    /// The editor previews the retained curve even during global bypass.
    public func responseDB(at frequency: Double, includePreamp: Bool = true) -> Double {
        bands.reduce(includePreamp ? preamp_db : 0) { $0 + $1.responseDB(at: frequency) }
    }
    public func responseDB(at frequencies: [Double]) -> [Double] {
        let coefficients = bands.filter(\.enabled).flatMap(\.sections)
        return frequencies.map { frequency in
            coefficients.reduce(preamp_db) { $0 + $1.responseDB(at: frequency) }
        }
    }
    /// Shared frequencies include exact centers and refined maxima/minima from both curves.
    /// Coefficients are compiled once per curve, never once per plotted point.
    public func responsePlot(selectedBand: Int? = nil) -> EqualizerResponsePlot {
        let enabledBands = bands.filter(\.enabled)
        let combined = enabledBands.flatMap(\.sections)
        let centers = enabledBands.map(\.frequency_hz)
        let selected = selectedBand.flatMap { bands.indices.contains($0) ? bands[$0] : nil }
        let selectedSections = selected.map { $0.enabled ? $0.sections : [] }
        var frequencies = EqualizerPlotSampling.frequencies
        frequencies.append(contentsOf: centers)
        frequencies.append(contentsOf: responseExtrema(combined, centers: centers, includingMinima: true).map(\.frequency))
        if let selected, let selectedSections {
            frequencies.append(selected.frequency_hz)
            frequencies.append(contentsOf: responseExtrema(selectedSections, centers: [selected.frequency_hz], includingMinima: true).map(\.frequency))
        }
        frequencies = Array(Set(frequencies)).sorted()
        let gains = frequencies.map { frequency in combined.reduce(preamp_db) { $0 + $1.responseDB(at: frequency) } }
        let selectedGains = selectedSections.map { sections in
            frequencies.map { frequency in sections.reduce(0) { $0 + $1.responseDB(at: frequency) } }
        }
        return EqualizerResponsePlot(frequencies: frequencies, combinedDB: gains, selectedDB: selectedGains)
    }
    /// Dense logarithmic sampling plus exact centers and local golden-section refinement.
    /// This estimates steady-state magnitude, not inter-sample or transient clipping.
    public var peakGainDB: Double {
        let sections = bands.filter(\.enabled).flatMap(\.sections)
        guard !sections.isEmpty else { return 0 }
        return responseExtrema(sections, centers: bands.filter(\.enabled).map(\.frequency_hz), includingMinima: false)
            .map(\.gainDB).max() ?? 0
    }
    public var recommendedPreampDB: Double { max(-96, min(0, -peakGainDB)) }
}

public struct EqualizerResponsePlot: Sendable {
    public let frequencies: [Double]
    public let combinedDB: [Double]
    public let selectedDB: [Double]?
}

private enum EqualizerPlotSampling {
    static let frequencies = (0...320).map { 20 * pow(1000, Double($0) / 320) }
    static let analysisFrequencies = (0...2048).map { 20 * pow(1000, Double($0) / 2048) }
}

/// Return endpoints, exact centers, and locally refined extrema. Shared by graph and headroom.
private func responseExtrema(_ sections: [EqualizerSection], centers: [Double], includingMinima: Bool)
    -> [(frequency: Double, gainDB: Double)] {
    guard !sections.isEmpty else { return [(20, 0), (20000, 0)] }
    func response(_ frequency: Double) -> Double {
        sections.reduce(0) { $0 + $1.responseDB(at: frequency) }
    }
    let points = Array(Set(EqualizerPlotSampling.analysisFrequencies + centers)).sorted()
    let values = points.map(response)
    var extrema = centers.map { (frequency: $0, gainDB: response($0)) }
    extrema.append((points[0], values[0]))
    extrema.append((points[points.count - 1], values[values.count - 1]))
    for i in 1..<(points.count - 1) {
        let isMaximum = values[i] >= values[i - 1] && values[i] >= values[i + 1]
            && (values[i] > values[i - 1] || values[i] > values[i + 1])
        let isMinimum = includingMinima && values[i] <= values[i - 1] && values[i] <= values[i + 1]
            && (values[i] < values[i - 1] || values[i] < values[i + 1])
        guard isMaximum || isMinimum else { continue }
        extrema.append((points[i], values[i]))
        var left = log(points[i - 1]), right = log(points[i + 1])
        for _ in 0..<32 {
            let x = right - (right - left) * 0.6180339887498949
            let y = left + (right - left) * 0.6180339887498949
            let xValue = response(exp(x)), yValue = response(exp(y))
            if isMaximum ? xValue > yValue : xValue < yValue { right = y } else { left = x }
        }
        let frequency = exp((left + right) / 2)
        extrema.append((frequency, response(frequency)))
    }
    return extrema
}

public struct EqualizerUserPreset: Codable, Sendable, Equatable, Identifiable {
    public let name: String
    public let equalizer: EqualizerSettings
    /// Rust compares names by their UTF-8 bytes, not Swift's canonical Unicode equivalence.
    public let id: Data
    public init(name: String, equalizer: EqualizerSettings) {
        self.name = name
        self.equalizer = equalizer
        id = Data(name.utf8)
    }
    private enum CodingKeys: String, CodingKey { case name, equalizer }
    public init(from decoder: any Decoder) throws {
        try requireKeys(decoder, ["name", "equalizer"])
        let values = try decoder.container(keyedBy: CodingKeys.self)
        name = try values.decode(String.self, forKey: .name)
        equalizer = try values.decode(EqualizerSettings.self, forKey: .equalizer)
        id = Data(name.utf8)
        guard Self.isValidName(name) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid equalizer preset name"))
        }
    }
    public func matchesName(_ candidate: String) -> Bool { name.utf8.elementsEqual(candidate.utf8) }
    public static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.equalizer == rhs.equalizer
    }
    public static func isValidName(_ name: String) -> Bool {
        !name.isEmpty && name.unicodeScalars.count <= 80
            && name.unicodeScalars.first?.properties.isWhitespace != true
            && name.unicodeScalars.last?.properties.isWhitespace != true
            && !name.unicodeScalars.contains { $0.value <= 0x1f || (0x7f...0x9f).contains($0.value) }
    }
}

public enum EqualizerPreset: String, CaseIterable, Sendable {
    case flat = "Flat", bassBoost = "Bass Boost", trebleBoost = "Treble Boost", vocal = "Vocal"

    /// Preserve the original graphic presets, including the warped one-octave bandwidth at 16 kHz.
    public func settings(enabled: Bool) -> EqualizerSettings {
        let gains: [Double]
        let preamp: Double
        switch self {
        case .flat: return EqualizerSettings(enabled: enabled)
        case .bassBoost: preamp = -6; gains = [5, 5, 3, 1, 0, 0, 0, 0, 0, 0]
        case .trebleBoost: preamp = -6; gains = [0, 0, 0, 0, 0, 0, 1, 3, 5, 5]
        case .vocal: preamp = -4; gains = [-2, -2, -1, 0, 1, 3, 3, 1, 0, -1]
        }
        let centers: [Double] = [31.25, 62.5, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
        return EqualizerSettings(enabled: enabled, preamp_db: preamp, bands: zip(centers, gains).map { f, gain in
            let w = 2 * Double.pi * f / 48000
            return EqualizerBand(frequency_hz: f, gain_db: gain, q: 1 / (2 * sinh(log(2) / 2 * w / sin(w))))
        })
    }
}
