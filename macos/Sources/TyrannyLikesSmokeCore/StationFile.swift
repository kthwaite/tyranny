import Foundation

public struct StationFileError: Error, Equatable {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }
}

/// Reads `.pls` and `.m3u`/`.m3u8` files into station inputs.
public enum StationFile {
    public static func parse(_ text: String, fileName: String) throws -> [StationInput] {
        let lines = text.replacingOccurrences(of: "\u{FEFF}", with: "")
            .components(separatedBy: .newlines)
        let candidates: [StationInput]
        switch (fileName as NSString).pathExtension.lowercased() {
        case "pls": candidates = parsePLS(lines)
        case "m3u", "m3u8": candidates = parseM3U(lines)
        default: throw StationFileError("\(fileName) is not a .pls or .m3u station file")
        }
        var seen = Set<String>()
        let stations = candidates.filter { seen.insert($0.url).inserted }
        if stations.isEmpty {
            throw StationFileError("\(fileName) has no http or https stations")
        }
        return stations
    }

    private static func isHTTP(_ url: String) -> Bool {
        let lowered = url.lowercased()
        return lowered.hasPrefix("http://") || lowered.hasPrefix("https://")
    }

    private static func trimmed(_ text: some StringProtocol) -> String {
        text.trimmingCharacters(in: .whitespaces)
    }

    private static func parsePLS(_ lines: [String]) -> [StationInput] {
        var files: [Int: String] = [:]
        var titles: [Int: String] = [:]
        for line in lines {
            guard let equals = line.firstIndex(of: "=") else { continue }
            let key = trimmed(line[..<equals]).lowercased()
            let value = trimmed(line[line.index(after: equals)...])
            if key.hasPrefix("file"), let number = Int(key.dropFirst(4)) {
                files[number] = value
            } else if key.hasPrefix("title"), let number = Int(key.dropFirst(5)) {
                titles[number] = value
            }
        }
        return files.keys.sorted().compactMap { number in
            guard let url = files[number], isHTTP(url) else { return nil }
            return StationInput(name: titles[number] ?? "", url: url)
        }
    }

    private static func parseM3U(_ lines: [String]) -> [StationInput] {
        var stations: [StationInput] = []
        var title = ""
        for raw in lines {
            let line = trimmed(raw)
            if line.isEmpty { continue }
            if line.hasPrefix("#") {
                if line.uppercased().hasPrefix("#EXTINF:"), let comma = line.firstIndex(of: ",") {
                    title = trimmed(line[line.index(after: comma)...])
                }
                continue
            }
            if isHTTP(line) {
                stations.append(StationInput(name: title, url: line))
            }
            title = ""
        }
        return stations
    }
}
