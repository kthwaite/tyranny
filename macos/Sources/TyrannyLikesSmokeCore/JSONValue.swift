import Foundation

public enum JSONValue: Codable, Sendable, Equatable {
    case string(String)
    case uint(UInt64)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case uints([UInt64])
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    /// Converts any `Encodable` to a `JSONValue`. Integers keep full 64-bit precision.
    public static func encoding<T: Encodable>(_ value: T) throws -> JSONValue {
        let data = try JSONEncoder().encode(value)
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }

    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ stringValue: String) { self.stringValue = stringValue }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    public init(from decoder: any Decoder) throws {
        if let keyed = try? decoder.container(keyedBy: Key.self) {
            var fields: [String: JSONValue] = [:]
            for key in keyed.allKeys { fields[key.stringValue] = try keyed.decode(JSONValue.self, forKey: key) }
            self = .object(fields)
            return
        }
        if var unkeyed = try? decoder.unkeyedContainer() {
            var items: [JSONValue] = []
            while !unkeyed.isAtEnd { items.append(try unkeyed.decode(JSONValue.self)) }
            self = .array(items)
            return
        }
        let single = try decoder.singleValueContainer()
        if single.decodeNil() { self = .null }
        else if let value = try? single.decode(Bool.self) { self = .bool(value) }
        else if let value = try? single.decode(UInt64.self) { self = .uint(value) }
        else if let value = try? single.decode(Int.self) { self = .int(value) }
        else if let value = try? single.decode(Double.self) { self = .double(value) }
        else { self = .string(try single.decode(String.self)) }
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .object(let fields):
            var container = encoder.container(keyedBy: Key.self)
            for (name, value) in fields { try container.encode(value, forKey: Key(name)) }
        case .array(let items):
            var container = encoder.unkeyedContainer()
            for item in items { try container.encode(item) }
        case .string(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .uint(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .int(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .double(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .uints(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .bool(let value): var c = encoder.singleValueContainer(); try c.encode(value)
        case .null: var c = encoder.singleValueContainer(); try c.encodeNil()
        }
    }
}
