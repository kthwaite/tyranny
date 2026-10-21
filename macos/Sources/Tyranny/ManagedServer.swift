import Darwin
import Foundation

/// Per-login launchd ownership. The daemon owns playback; the app only pauses it on quit.
@MainActor
final class ManagedServer {
    let endpoint: String
    let tokenPath: String
    let label: String
    let logPath: String
    private let state: URL
    private let agentDirectory: URL
    private let helper: URL
    private let bind: String
    private let capture: String?
    private let home: String
    private var operation = false
    private var service: String { "gui/\(getuid())/\(label)" }
    private var plist: URL { agentDirectory.appendingPathComponent("\(label).plist") }

    private struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static func make(arguments: [String] = CommandLine.arguments) throws -> ManagedServer? {
        let names = ["--managed-state-dir", "--managed-bind", "--managed-label", "--managed-agent-dir", "--managed-capture"]
        var values: [String: String] = [:]
        for (index, argument) in arguments.enumerated() where names.contains(argument) {
            guard values[argument] == nil, index + 1 < arguments.count,
                  !arguments[index + 1].hasPrefix("--") else {
                throw Failure(message: "\(argument) requires exactly one value.")
            }
            values[argument] = arguments[index + 1]
        }
        let unmanaged = arguments.contains("--url") || arguments.contains("--token-file")
        if values.isEmpty && unmanaged { return nil }
        guard !unmanaged else { throw Failure(message: "Do not combine --url/--token-file with managed server options.") }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let custom = !values.isEmpty
        if custom {
            guard names.prefix(4).allSatisfy({ values[$0] != nil }) else {
                throw Failure(message: "Isolated management requires --managed-state-dir, --managed-bind, --managed-label and --managed-agent-dir together.")
            }
        }
        let label = values["--managed-label"] ?? "dev.tyranny.server"
        guard !custom || (label.hasPrefix("dev.tyranny.spike.") && label.count > 18 && label.count < 128 && label.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "." || $0 == "-") })) else {
            throw Failure(message: "An isolated label must be dev.tyranny.spike.UNIQUE (ASCII letters, digits, dots or hyphens).")
        }
        let bind = values["--managed-bind"] ?? "127.0.0.1:9865"
        guard bind.hasPrefix("127.0.0.1:"), let port = UInt16(bind.dropFirst(10)), port > 0,
              bind == "127.0.0.1:\(port)" else {
            throw Failure(message: "Managed server bind must be literal 127.0.0.1:PORT, with PORT from 1 through 65535.")
        }
        func path(_ value: String) throws -> URL {
            guard value.hasPrefix("/"), !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
                  !value.split(separator: "/").contains("..") else {
                throw Failure(message: "Managed paths must be absolute, without parent traversal or control characters.")
            }
            let url = URL(fileURLWithPath: value).standardizedFileURL
            guard url.path != "/" else { throw Failure(message: "The filesystem root cannot be a managed directory.") }
            return url
        }
        let state = try path(values["--managed-state-dir"] ?? "\(home)/Library/Application Support/tyranny")
        let agents = try path(values["--managed-agent-dir"] ?? "\(home)/Library/LaunchAgents")
        if custom {
            let defaultState = "\(home)/Library/Application Support/tyranny"
            let defaultAgents = "\(home)/Library/LaunchAgents"
            guard state.path != defaultState, !state.path.hasPrefix(defaultState + "/"),
                  agents.path != defaultAgents, !agents.path.hasPrefix(defaultAgents + "/") else {
                throw Failure(message: "Isolated overrides must not use the normal state or login-agent directories.")
            }
        }
        let capture = try values["--managed-capture"].map { try path($0).path }
        if let capture {
            guard capture.hasPrefix(state.path + "/"), capture.hasSuffix(".f32"),
                  URL(fileURLWithPath: capture).deletingLastPathComponent().path == state.path else {
                throw Failure(message: "Managed capture must be a .f32 file directly inside the isolated state directory.")
            }
        }
        return ManagedServer(state: state, agents: agents, bind: bind, label: label, capture: capture, home: home)
    }

    private init(state: URL, agents: URL, bind: String, label: String, capture: String?, home: String) {
        self.state = state
        agentDirectory = agents
        self.bind = bind
        self.label = label
        self.capture = capture
        self.home = home
        helper = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/tyranny-server")
        endpoint = "ws://\(bind)/ws"
        tokenPath = state.appendingPathComponent("token").path
        logPath = state.appendingPathComponent("server.log").path
    }

    func ensureRunning(replacingIncompatibleServer: Bool = false) async throws {
        try begin()
        defer { operation = false }
        // A manually started, authenticated daemon is always preferable to a new job.
        if await ready() { return }
        try Task.checkCancellation()
        try prepare()
        try await preflight()
        if replacingIncompatibleServer, !(try await ownedLoadedJob()) {
            throw failure("The incompatible server is not a managed Tyranny job; it was left running. Update that server separately.")
        }
        try await installAndStart()
        try await waitUntilReady()
    }

    func restart() async throws {
        try begin()
        defer { operation = false }
        try prepare()
        guard try ownedPlist() != nil else {
            throw failure("No owned LaunchAgent exists. A manual server will not be restarted; stop it using its original owner.")
        }
        try await preflight()
        if try await ownedLoadedJob() {
            // bootout gives the Rust owner SIGTERM and its checkpoint window; -k is a kill.
            try await stopOwnedJob()
        } else if await ready() {
            throw failure("A manual server is using this endpoint; it will not be replaced.")
        }
        try await installAndStart()
        try await waitUntilReady()
    }

    func uninstall() async throws {
        try begin()
        defer { operation = false }
        try Self.checkPath(agentDirectory, create: false, privateLeaf: false)
        guard let existing = try ownedPlist() else { return }
        if try await ownedLoadedJob() { try await stopOwnedJob() }
        // Recheck immediately before removing; never remove a replacement configuration.
        guard let current = try ownedPlist() else { return }
        guard NSDictionary(dictionary: current).isEqual(to: existing) else {
            throw failure("LaunchAgent configuration changed during removal; Retry.")
        }
        try FileManager.default.removeItem(at: plist)
    }

    private func begin() throws {
        guard !operation else { throw failure("Another background-server operation is in progress.") }
        operation = true
    }

    private func failure(_ text: String) -> Failure {
        Failure(message: "\(text) Server log: \(logPath)")
    }

    private var environment: [String: String] {
        ["HOME": home, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"]
    }

    private var helperArguments: [String] {
        var result = [helper.path, "--state-dir", state.path, "--bind", bind]
        if let capture { result += ["--capture", capture] }
        return result
    }

    private var job: [String: Any] {
        // env -i removes inherited login-shell/DYLD settings; every executable path is absolute.
        ["Label": label, "Program": "/usr/bin/env",
         "ProgramArguments": ["/usr/bin/env", "-i", "HOME=\(home)", "PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LANG=en_US.UTF-8"] + helperArguments,
         "RunAtLoad": true, "KeepAlive": true, "ThrottleInterval": 10, "ExitTimeOut": 15,
         "Umask": 63, "StandardOutPath": logPath, "StandardErrorPath": logPath,
         "WorkingDirectory": state.path, "ProcessType": "Interactive",
         "AssociatedBundleIdentifiers": [Bundle.main.bundleIdentifier ?? "dev.tyranny.spike"]]
    }

    private func prepare() throws {
        try Self.checkPath(state, create: true, privateLeaf: true)
        try Self.checkPath(agentDirectory, create: true, privateLeaf: false)
        try Self.checkPath(helper.deletingLastPathComponent(), create: false, privateLeaf: false)
        try Self.checkFile(helper.path, executable: true)
        let fd = try Self.privateFile(logPath)
        close(fd)
        if let capture {
            let fd = try Self.privateFile(capture)
            close(fd)
        }
    }

    private func ownedPlist() throws -> [String: Any]? {
        var info = stat()
        if lstat(plist.path, &info) != 0 {
            if errno == ENOENT { return nil }
            throw failure("Cannot inspect LaunchAgent configuration.")
        }
        try Self.checkFile(plist.path)
        let data = try Data(contentsOf: plist)
        let desired = job
        guard let desiredArguments = desired["ProgramArguments"] as? [String],
              let existing = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              let arguments = existing["ProgramArguments"] as? [String],
              arguments.count == desiredArguments.count else {
            throw failure("Unrecognized LaunchAgent configuration at \(plist.path); it was left unchanged.")
        }
        // A moved/replaced app still owns the same job. Only the bundle location
        // may differ: environment, state, endpoint, logs and launchd policy must match.
        let oldHelper = arguments[5]
        let oldURL = URL(fileURLWithPath: oldHelper)
        guard oldHelper.hasPrefix("/"), oldURL.standardizedFileURL.path == oldHelper,
              !oldHelper.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              oldURL.lastPathComponent == "tyranny-server",
              oldURL.deletingLastPathComponent().lastPathComponent == "MacOS",
              oldURL.deletingLastPathComponent().deletingLastPathComponent().lastPathComponent == "Contents",
              oldURL.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().pathExtension == "app" else {
            throw failure("The LaunchAgent does not reference a bundled Tyranny server; it was left unchanged.")
        }
        var expected = desired
        var expectedArguments = desiredArguments
        expectedArguments[5] = oldHelper
        expected["ProgramArguments"] = expectedArguments
        guard NSDictionary(dictionary: existing).isEqual(to: expected) else {
            throw failure("The LaunchAgent at \(plist.path) is not a recognized Tyranny configuration; it was left unchanged.")
        }
        return existing
    }

    private func ownedLoadedJob() async throws -> Bool {
        let result = try await command("/bin/launchctl", ["print", service])
        guard result.status == 0 else {
            // Distinguish a missing job from a missing/unavailable GUI domain.
            try await launchctl(["print", "gui/\(getuid())"])
            return false
        }
        guard let existing = try ownedPlist() else { throw failure("The label is already registered without our owned plist; refusing to change it.") }
        // Match the loaded job to its saved configuration, not the new bundle.
        // Fail closed if launchctl's diagnostic format changes.
        let origins = result.text.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        let start = origins.firstIndex(of: "arguments = {")
        let loadedArguments = start.map { Array(origins.dropFirst($0 + 1).prefix(while: { $0 != "}" })) }
        guard origins.contains("path = \(plist.path)"), origins.contains("program = /usr/bin/env"),
              loadedArguments == existing["ProgramArguments"] as? [String] else {
            throw failure("Cannot establish the loaded job's origin. Refusing to change an unrelated or unrecognized LaunchAgent.")
        }
        return true
    }

    private func installAndStart() async throws {
        let existing = try ownedPlist()
        if try await ownedLoadedJob() {
            // This path is reached only when readiness failed or restart was
            // requested. Gracefully retire even a same-path, older server binary.
            try await stopOwnedJob()
        }
        let current = try ownedPlist()
        guard NSDictionary(dictionary: current ?? [:]).isEqual(to: existing ?? [:]) else {
            throw failure("LaunchAgent configuration changed during startup; Retry.")
        }
        let needsInstall = current.map { !NSDictionary(dictionary: $0).isEqual(to: job) } ?? true
        if current != nil && needsInstall {
            // The old bundle may already be gone. Never execute it or require it
            // to uninstall itself; retain the user's state and replace only its job.
            try FileManager.default.removeItem(at: plist)
        }
        if needsInstall {
            let data = try PropertyListSerialization.data(fromPropertyList: job, format: .xml, options: 0)
            // O_EXCL ensures an intervening owner cannot be overwritten.
            let fd = open(plist.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
            guard fd >= 0 else { throw failure("Cannot exclusively create \(plist.path).") }
            let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            do { try handle.write(contentsOf: data); try handle.close() }
            catch { try? handle.close(); throw error }
        }
        try await launchctl(["bootstrap", "gui/\(getuid())", plist.path])
    }

    private func preflight() async throws {
        let result = try await command(helper.path, ["--help"], log: state.appendingPathComponent("helper-check.log"))
        guard result.status == 0, result.text.contains("tyranny-server") else {
            throw failure("The bundled server cannot start (exit \(result.status)). Reinstall the app or rebuild it with scripts/build_macos.sh release. Release apps include their decoder libraries. Details: \(state.appendingPathComponent("helper-check.log").path). \(result.text.prefix(3000))")
        }
    }

    private func ready() async -> Bool {
        let client = WireClient()
        defer { client.disconnect() }
        do { try await client.connect(endpoint: endpoint, tokenPath: tokenPath); return true }
        catch { return false }
    }

    private func waitUntilReady() async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(30))
        repeat {
            try Task.checkCancellation()
            if await ready() { return }
            try await Task.sleep(for: .milliseconds(200))
        } while ContinuousClock.now < deadline
        throw failure("Background server did not complete an authenticated protocol-v4 handshake within 30 seconds. Check decoder dependencies, port conflicts, and folder permissions; then Retry.")
    }

    private func stopOwnedJob() async throws {
        try await launchctl(["bootout", service])
        let deadline = ContinuousClock.now.advanced(by: .seconds(20))
        repeat {
            let loaded = try await command("/bin/launchctl", ["print", service])
            if loaded.status != 0 {
                let responding = await ready()
                if !responding { return }
            }
            try await Task.sleep(for: .milliseconds(200))
        } while ContinuousClock.now < deadline
        throw failure("The removed job has not finished stopping. Its database and configuration were preserved; wait and Retry.")
    }

    private func launchctl(_ arguments: [String]) async throws {
        let result = try await command("/bin/launchctl", arguments)
        guard result.status == 0 else { throw failure("launchctl \(arguments.first ?? "") failed (\(result.status)): \(result.text.prefix(2000))") }
    }

    private struct CommandResult: Sendable { let status: Int32; let text: String }

    private func command(_ executable: String, _ arguments: [String], log: URL? = nil) async throws -> CommandResult {
        try Task.checkCancellation()
        let destination = log ?? state.appendingPathComponent("launchctl-\(UUID().uuidString).log")
        let environment = environment
        return try await Task.detached(priority: .utility) {
            let fd = try Self.privateFile(destination.path)
            let output = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            defer {
                try? output.close()
                if log == nil { try? FileManager.default.removeItem(at: destination) }
            }
            try output.truncate(atOffset: 0)
            let process = Process()
            process.executableURL = URL(fileURLWithPath: executable)
            process.arguments = arguments
            process.environment = environment
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = output
            process.standardError = output
            try process.run()
            let deadline = ContinuousClock.now.advanced(by: .seconds(20))
            while process.isRunning && ContinuousClock.now < deadline { usleep(20_000) }
            if process.isRunning {
                kill(process.processIdentifier, SIGKILL)
                process.waitUntilExit()
                throw Failure(message: "\(executable) timed out after 20 seconds. Details: \(destination.path)")
            }
            process.waitUntilExit()
            try output.seek(toOffset: 0)
            let data = try output.read(upToCount: 64 * 1024) ?? Data()
            return CommandResult(status: process.terminationStatus, text: String(decoding: data, as: UTF8.self))
        }.value
    }

    private nonisolated static func checkPath(_ path: URL, create: Bool, privateLeaf: Bool) throws {
        var current = URL(fileURLWithPath: "/", isDirectory: true)
        for component in path.pathComponents.dropFirst() {
            current.appendPathComponent(component, isDirectory: true)
            var info = stat()
            if lstat(current.path, &info) != 0 {
                guard errno == ENOENT else { throw Failure(message: "Cannot inspect \(current.path).") }
                if !create { return }
                guard mkdir(current.path, 0o700) == 0 || errno == EEXIST,
                      lstat(current.path, &info) == 0 else { throw Failure(message: "Cannot create \(current.path).") }
            }
            let rootSticky = info.st_uid == 0 && (info.st_mode & 0o1000) != 0
            guard (info.st_mode & S_IFMT) == S_IFDIR,
                  info.st_uid == getuid() || info.st_uid == 0,
                  (info.st_mode & 0o022) == 0 || rootSticky else {
                throw Failure(message: "Refusing an unsafe, writable or symlink directory: \(current.path).")
            }
            if current == path {
                guard info.st_uid == getuid(), !privateLeaf || (info.st_mode & 0o077) == 0 else {
                    throw Failure(message: "Managed state must be owned by you and private (chmod 700): \(path.path).")
                }
            }
        }
    }

    private nonisolated static func checkFile(_ path: String, executable: Bool = false) throws {
        var info = stat()
        guard lstat(path, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG,
              info.st_uid == getuid(), info.st_nlink == 1, (info.st_mode & 0o022) == 0,
              !executable || access(path, X_OK) == 0 else {
            throw Failure(message: "Refusing a missing, unsafe or symlink file: \(path). Rebuild the development bundle if its helper is missing.")
        }
    }

    private nonisolated static func privateFile(_ path: String) throws -> Int32 {
        let fd = open(path, O_RDWR | O_CREAT | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { throw Failure(message: "Cannot securely open \(path).") }
        var info = stat()
        guard fstat(fd, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG,
              info.st_uid == getuid(), info.st_nlink == 1, (info.st_mode & 0o022) == 0,
              fchmod(fd, 0o600) == 0 else {
            close(fd)
            throw Failure(message: "Refusing an unsafe private log/capture file: \(path).")
        }
        return fd
    }
}
