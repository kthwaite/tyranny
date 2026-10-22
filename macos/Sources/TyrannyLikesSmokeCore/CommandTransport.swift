import Foundation

public struct CommandRequest: Equatable, Sendable {
    public let name: String
    public let fields: [String: JSONValue]

    init(_ name: String, _ fields: [String: JSONValue] = [:]) {
        self.name = name
        self.fields = fields
    }
}

extension CommandLanguage {
    /// Resolves only transport commands. The caller sends the result through its normal
    /// mutation path; the entry ID keeps a seek safe if playback advances before delivery.
    public static func transportRequest(_ command: ParsedCommand, state: ControlState) throws -> CommandRequest? {
        switch command.id {
        case .play, .toggle:
            guard state.queue_length > 0 else { throw CommandLanguageError("Up Next is empty.") }
            return command.id == .play ? CommandRequest("play", ["entry_id": .null]) : CommandRequest("toggle_play_pause")
        case .pause:
            guard state.current_id != nil else { throw CommandLanguageError("There is no current entry to pause.") }
            return CommandRequest("pause")
        case .stop: return CommandRequest("stop")
        case .next:
            guard state.can_next else { throw CommandLanguageError("There is no next entry.") }
            return CommandRequest("next")
        case .previous:
            guard state.can_previous else { throw CommandLanguageError("There is no previous entry.") }
            return CommandRequest("previous")
        case .seek:
            guard let entryID = state.current_id, let current = state.now_playing,
                  current.entry_id == entryID, state.sample_rate > 0,
                  let duration = current.duration_frames, duration > 0 else {
                throw CommandLanguageError("Seeking requires a current entry with a known duration and sample rate.")
            }
            let value = try quantity(command.arguments[0], units: ["s", "m", "h", "%"])
            let frames: Decimal
            if value.unit == "%" {
                frames = try calculate(try calculate(value.magnitude, Decimal(duration), using: NSDecimalMultiply), 100, using: NSDecimalDivide)
            } else {
                let secondsPerUnit: Decimal = value.unit == "h" ? 3600 : value.unit == "m" ? 60 : 1
                frames = try calculate(try calculate(value.magnitude, secondsPerUnit, using: NSDecimalMultiply), Decimal(state.sample_rate), using: NSDecimalMultiply)
            }
            guard frames <= Decimal(UInt64.max) else { throw CommandLanguageError("Seek value overflows the frame range.") }
            let target: Decimal
            if value.relative {
                target = try calculate(Decimal(state.position_frames), frames, using: value.negative ? NSDecimalSubtract : NSDecimalAdd)
            } else {
                target = frames
            }
            guard target <= Decimal(UInt64.max) else { throw CommandLanguageError("Seek position overflows the frame range.") }
            let clamped = min(max(target, 0), Decimal(duration - 1))
            var input = clamped
            var rounded = Decimal()
            NSDecimalRound(&rounded, &input, 0, .down)
            guard let position = UInt64(NSDecimalNumber(decimal: rounded).stringValue) else {
                throw CommandLanguageError("Seek position cannot be represented as frames.")
            }
            return CommandRequest("seek", ["entry_id": .uint(entryID), "position_frames": .uint(position)])
        case .volume:
            let value = try quantity(command.arguments[0], units: ["%"])
            let target = value.relative
                ? try calculate(Decimal(state.settings.volume), value.magnitude, using: value.negative ? NSDecimalSubtract : NSDecimalAdd)
                : value.magnitude
            var clamped = min(max(target, 0), 100)
            var rounded = Decimal()
            // Volume is integral on the wire; round to the nearest percent after adjustment.
            NSDecimalRound(&rounded, &clamped, 0, .plain)
            return CommandRequest("set_volume", ["percent": .int(NSDecimalNumber(decimal: rounded).intValue)])
        case .repeatMode:
            let requested = command.arguments[0]
            let mode: String
            if requested == "cycle" {
                switch state.settings.repeat {
                case .off: mode = "all"
                case .all: mode = "one"
                case .one: mode = "off"
                }
            } else { mode = requested }
            return CommandRequest("set_repeat", ["mode": .string(mode)])
        case .shuffle:
            return CommandRequest("set_shuffle", ["enabled": .bool(enabled(command.arguments[0], current: state.settings.shuffle))])
        case .gain:
            return CommandRequest("set_gain", ["mode": .string(command.arguments[0])])
        case .resume:
            return CommandRequest("set_resume", ["enabled": .bool(enabled(command.arguments[0], current: state.settings.resume_on_start))])
        default: return nil
        }
    }

    private static func enabled(_ value: String, current: Bool) -> Bool {
        value == "toggle" ? !current : value == "on"
    }

    /// Decimal arithmetic preserves authoritative 64-bit frame positions instead of
    /// rounding them through Double, including relative offsets beyond 2^53 frames.
    private static func calculate(
        _ lhs: Decimal, _ rhs: Decimal,
        using operation: (UnsafeMutablePointer<Decimal>, UnsafePointer<Decimal>, UnsafePointer<Decimal>, Decimal.RoundingMode) -> Decimal.CalculationError
    ) throws -> Decimal {
        var left = lhs
        var right = rhs
        var result = Decimal()
        guard operation(&result, &left, &right, .plain) == .noError, !result.isNaN else {
            throw CommandLanguageError("Numeric operation overflows or loses precision.")
        }
        return result
    }
}

/// One request frame as the client sends it: the command's fields plus `id` and `command`.
public enum RequestFrame {
    /// The most bytes one frame may hold. The server refuses larger frames (`MAX_REQUEST_BYTES`).
    public static let limit = 16 * 1024

    public static func encode(_ command: String, fields: [String: JSONValue], id: UInt64) throws -> Data {
        var object = fields
        object["id"] = .uint(id)
        object["command"] = .string(command)
        return try JSONEncoder().encode(object)
    }
}
