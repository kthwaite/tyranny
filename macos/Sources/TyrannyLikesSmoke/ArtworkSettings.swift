import Foundation
import Observation
import Security
import TyrannyLikesSmokeCore

@MainActor @Observable
final class ArtworkSettings {
    static let preferencesKey = "artworkProviders"
    private(set) var preferences: ArtworkPreferences
    private(set) var hasToken = false
    private(set) var error: String?
    @ObservationIgnored var onChange: (() -> Void)?
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var token: String?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let data = defaults.data(forKey: Self.preferencesKey),
           let saved = try? JSONDecoder().decode(ArtworkPreferences.self, from: data) {
            preferences = ArtworkPreferences(order: saved.order, enabled: saved.enabled)
        } else { preferences = ArtworkPreferences() }
        do {
            token = try DiscogsKeychain.read()
            hasToken = token != nil
            if preferences.enabled.contains(.discogs) { error = ArtworkPreferences.tokenError(token ?? "") }
        } catch { self.error = "Cannot read the Discogs token from Keychain. Unlock your login keychain and save the token again." }
    }

    var requestFields: [String: JSONValue] {
        // A locked/missing credential never blocks local art or the other enabled providers.
        let providers = preferences.providers.filter { $0 != .discogs || ArtworkPreferences.tokenError(token ?? "") == nil }
        return ["providers": .array(providers.map { .string($0.rawValue) }),
                "discogs_token": providers.contains(.discogs) ? .string(token!) : .null]
    }

    func setEnabled(_ provider: ArtworkProvider, _ enabled: Bool) {
        if provider == .discogs, enabled, let problem = ArtworkPreferences.tokenError(token ?? "") {
            error = problem
            return
        }
        error = nil
        preferences.setEnabled(provider, enabled)
        persist()
    }

    func reportCredentialFailure() {
        error = "Discogs rejected credentials or access. Check your personal token and Discogs account permissions."
    }

    func move(_ provider: ArtworkProvider, by offset: Int) {
        preferences.move(provider, by: offset)
        persist()
    }

    @discardableResult func saveToken(_ value: String) -> Bool {
        if let problem = ArtworkPreferences.tokenError(value) { error = problem; return false }
        do {
            try DiscogsKeychain.save(value)
            token = value
            hasToken = true
            error = nil
            onChange?()
            return true
        } catch {
            self.error = "Cannot save the Discogs token in Keychain. Unlock your login keychain and try again."
            return false
        }
    }

    func removeToken() {
        do {
            try DiscogsKeychain.remove()
            token = nil
            hasToken = false
            error = nil
            preferences.setEnabled(.discogs, false)
            persist()
        } catch { self.error = "Cannot remove the Discogs token from Keychain." }
    }

    private func persist() {
        if let data = try? JSONEncoder().encode(preferences) { defaults.set(data, forKey: Self.preferencesKey) }
        onChange?()
    }
}

private enum DiscogsKeychain {
    private struct Failure: Error {}
    private static var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: (Bundle.main.bundleIdentifier ?? "app.tyranny") + ".artwork.discogs",
         kSecAttrAccount as String: "personal-access-token"]
    }

    static func read() throws -> String? {
        var query = query
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data,
              let token = String(data: data, encoding: .utf8) else { throw Failure() }
        return token
    }

    static func save(_ token: String) throws {
        let data = Data(token.utf8)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw Failure() }
        } else if status != errSecSuccess { throw Failure() }
    }

    static func remove() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure() }
    }
}
