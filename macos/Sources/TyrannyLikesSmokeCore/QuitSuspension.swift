import Foundation

/// Recorded at quit when playback was running, so the next launch can resume it.
public struct QuitSuspension: Codable, Equatable, Sendable {
    public let socketPath: String
    public let entryID: UInt64

    public init(socketPath: String, entryID: UInt64) {
        self.socketPath = socketPath
        self.entryID = entryID
    }

    /// Non-nil only when `transport` is "playing" or "buffering" and `currentID` is set.
    public static func note(socketPath: String, transport: String?, currentID: UInt64?) -> QuitSuspension? {
        guard transport == "playing" || transport == "buffering", let currentID else { return nil }
        return QuitSuspension(socketPath: socketPath, entryID: currentID)
    }

    /// True only for the same socket, `transport == "paused"`, and `currentID == entryID`.
    public func shouldResume(socketPath: String, transport: String?, currentID: UInt64?) -> Bool {
        self.socketPath == socketPath && transport == "paused" && currentID == entryID
    }
}

public struct QuitSuspensionStore: Sendable {
    public static let key = "quitSuspension"
    /// The defaults suite for gate and render runs, so they never read or write the user's note.
    public static let harnessSuiteName = "dev.tyranny-likes-smoke.controller.harness"

    /// `harnessSuiteName` for a gate, smoke, render or isolated-management launch; nil (the
    /// standard defaults) for a normal launch. Every launch of one harness run shares the suite,
    /// so a note written by one launch is read by the next.
    public static func suiteName(forArguments arguments: [String]) -> String? {
        let harness = arguments.dropFirst().contains { argument in
            ["--render-views", "--lifecycle-output", "--managed-state-dir", "--model-gate-output",
             "--scale-output"].contains(argument)
                || (argument.hasPrefix("--") && argument.contains("smoke"))
        }
        return harness ? harnessSuiteName : nil
    }

    /// The store for a launch with `arguments`; see `suiteName(forArguments:)`.
    public static func forLaunch(arguments: [String]) -> QuitSuspensionStore {
        let defaults = suiteName(forArguments: arguments).flatMap(UserDefaults.init(suiteName:)) ?? .standard
        return QuitSuspensionStore(defaults: defaults)
    }

    // UserDefaults is documented thread-safe, so sharing it across isolation domains is sound.
    nonisolated(unsafe) private let defaults: UserDefaults

    public init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    public func save(_ note: QuitSuspension) {
        guard let data = try? JSONEncoder().encode(note) else { return }
        defaults.set(data, forKey: Self.key)
    }

    /// Returns the stored note and removes it. Undecodable data is removed and returns nil.
    public func take() -> QuitSuspension? {
        guard let object = defaults.object(forKey: Self.key) else { return nil }
        defaults.removeObject(forKey: Self.key)
        guard let data = object as? Data else { return nil }
        return try? JSONDecoder().decode(QuitSuspension.self, from: data)
    }
}

public enum Deadline {
    /// The operation's result, or nil once `limit` passes. It returns at the deadline even
    /// if the operation ignores cancellation; the operation is cancelled but not awaited.
    public static func race<T: Sendable>(_ limit: Duration,
                                         _ operation: @escaping @Sendable () async throws -> T) async -> T? {
        let gate = Gate<T>()
        return await withCheckedContinuation { (continuation: CheckedContinuation<T?, Never>) in
            gate.install(continuation)
            let work = Task {
                let result = try? await operation()
                gate.finish(result)
            }
            let sleeper = Task {
                try? await Task.sleep(for: limit)
                gate.finish(nil)
            }
            gate.setTasks([work, sleeper])
        }
    }

    /// Resumes the continuation once, from whichever side finishes first, then cancels both tasks.
    private final class Gate<T: Sendable>: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<T?, Never>?
        private var tasks: [Task<Void, Never>] = []
        private var resumed = false

        func install(_ continuation: CheckedContinuation<T?, Never>) {
            lock.withLock { self.continuation = continuation }
        }

        func setTasks(_ new: [Task<Void, Never>]) {
            let cancelNow = lock.withLock { () -> Bool in
                tasks = new
                return resumed
            }
            if cancelNow { new.forEach { $0.cancel() } }
        }

        func finish(_ value: T?) {
            let (toResume, toCancel) = lock.withLock { () -> (CheckedContinuation<T?, Never>?, [Task<Void, Never>]) in
                guard !resumed else { return (nil, []) }
                resumed = true
                let c = continuation
                continuation = nil
                return (c, tasks)
            }
            toResume?.resume(returning: value)
            toCancel.forEach { $0.cancel() }
        }
    }
}
