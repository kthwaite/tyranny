import Foundation

/// Four-byte unsigned big-endian length followed by a bounded JSON payload.
public enum JSONFrame {
    public enum Failure: Error, Equatable {
        case invalidHeader
        case invalidLength
    }

    public static func payloadLength(_ header: Data, limit: Int) throws -> Int {
        guard header.count == 4 else { throw Failure.invalidHeader }
        let length = header.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        guard length > 0, UInt64(length) <= UInt64(max(0, limit)) else {
            throw Failure.invalidLength
        }
        return Int(length)
    }

    public static func encode(_ payload: Data, limit: Int) throws -> Data {
        guard !payload.isEmpty, payload.count <= limit, let length = UInt32(exactly: payload.count) else {
            throw Failure.invalidLength
        }
        var frame = Data(capacity: 4 + payload.count)
        frame.append(UInt8(truncatingIfNeeded: length >> 24))
        frame.append(UInt8(truncatingIfNeeded: length >> 16))
        frame.append(UInt8(truncatingIfNeeded: length >> 8))
        frame.append(UInt8(truncatingIfNeeded: length))
        frame.append(payload)
        return frame
    }
}
