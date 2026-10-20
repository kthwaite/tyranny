import AppKit
import ImageIO
import Observation
import TyrannyCore
import UniformTypeIdentifiers
import UserNotifications

/// Owns opt-in, authorization and delivery. No network or artwork work runs on the playback path.
@MainActor @Observable
final class TrackNotifications: NSObject, UNUserNotificationCenterDelegate {
    private(set) var enabled: Bool
    var showWhileActive: Bool {
        didSet { defaults.set(showWhileActive, forKey: "notificationsWhileActive") }
    }
    private(set) var authorization = "Not requested"
    private(set) var error: String?
    private(set) var requesting = false
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var policy = TrackNotificationPolicy()
    @ObservationIgnored private var delivery: Task<Void, Never>?
    @ObservationIgnored private var ticket = UUID()
    @ObservationIgnored private var currentEntry: UInt64?
    @ObservationIgnored private var deliveredIdentifier: String?
    @ObservationIgnored private let attachmentDirectory = FileManager.default.temporaryDirectory
        .appendingPathComponent("tyranny-notifications-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)

    private var center: UNUserNotificationCenter? {
        // UserNotifications aborts for a command-line executable without an application bundle.
        guard Bundle.main.bundleURL.pathExtension == "app" else { return nil }
        return .current()
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        enabled = defaults.bool(forKey: "trackNotifications")
        showWhileActive = defaults.bool(forKey: "notificationsWhileActive")
        super.init()
        center?.delegate = self
    }

    func refreshAuthorization() async {
        guard let center else {
            authorization = "Requires the Tyranny app bundle"
            return
        }
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined: authorization = "Not requested"
        case .denied: authorization = "Denied — enable Tyranny in System Settings > Notifications"
        case .authorized: authorization = "Allowed"
        case .provisional: authorization = "Allowed quietly"
        @unknown default: authorization = "Unknown"
        }
    }

    func setEnabled(_ value: Bool) {
        enabled = value
        defaults.set(value, forKey: "trackNotifications")
        error = nil
        cancelDelivery()
        guard value else {
            if let deliveredIdentifier { center?.removeDeliveredNotifications(withIdentifiers: [deliveredIdentifier]) }
            deliveredIdentifier = nil
            return
        }
        guard let center else {
            error = "Launch the Tyranny app bundle to enable notifications."
            return
        }
        requesting = true
        Task {
            defer { requesting = false }
            do {
                let allowed = try await center.requestAuthorization(options: [.alert])
                if !allowed { error = "Notifications are disabled in System Settings." }
            } catch { self.error = error.localizedDescription }
            await refreshAuthorization()
        }
    }

    func receive(_ state: ControlState, newSession: Bool, artwork: ArtworkStore) {
        let entry = state.now_playing?.entry_id
        if entry != currentEntry || newSession || state.transport != "playing" {
            cancelDelivery()
            currentEntry = entry
        }
        guard policy.shouldNotify(entryID: entry, transport: state.transport, newSession: newSession,
                                  enabled: enabled, frontmost: NSApp.isActive, showWhileActive: showWhileActive),
              let track = state.now_playing, let center else { return }
        cancelDelivery()
        let generation = ticket
        delivery = Task { [weak self] in
            guard let self else { return }
            // Give existing artwork demand up to one second; never start another download.
            for _ in 0..<25 {
                guard !Task.isCancelled, ticket == generation else { return }
                if artwork.nowPlaying.state != "loading" { break }
                do { try await Task.sleep(for: .milliseconds(40)) } catch { return }
            }
            let settings = await center.notificationSettings()
            guard !Task.isCancelled, ticket == generation, enabled,
                  !NSApp.isActive || showWhileActive,
                  settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional else { return }
            let content = UNMutableNotificationContent()
            content.title = track.title
            content.subtitle = track.artist
            content.body = track.album
            if artwork.nowPlayingRequest?.target == .entry(track.entry_id), let image = artwork.nowPlaying.image {
                do { content.attachments = [try attachment(image)] }
                catch { self.error = "Notification artwork: \(error.localizedDescription)" }
            }
            let identifier = "tyranny.now-playing.\(generation.uuidString)"
            do {
                try await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
                if ticket != generation {
                    center.removeDeliveredNotifications(withIdentifiers: [identifier])
                } else {
                    if let deliveredIdentifier { center.removeDeliveredNotifications(withIdentifiers: [deliveredIdentifier]) }
                    deliveredIdentifier = identifier
                }
            } catch { self.error = error.localizedDescription }
        }
    }

    func disconnected() { cancelDelivery() }

    func shutdown() {
        cancelDelivery()
        try? FileManager.default.removeItem(at: attachmentDirectory)
    }

    private func cancelDelivery() {
        ticket = UUID()
        delivery?.cancel()
        delivery = nil
    }

    private func attachment(_ image: CGImage) throws -> UNNotificationAttachment {
        try FileManager.default.createDirectory(at: attachmentDirectory, withIntermediateDirectories: true)
        let url = attachmentDirectory.appendingPathComponent("artwork.jpg")
        guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil) else {
            throw CocoaError(.fileWriteUnknown)
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw CocoaError(.fileWriteUnknown) }
        return try UNNotificationAttachment(identifier: "artwork", url: url)
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        await MainActor.run { enabled && (!NSApp.isActive || showWhileActive) ? [.banner, .list] : [] }
    }
}
