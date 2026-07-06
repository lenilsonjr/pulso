import Foundation
import Observation

enum SettingsKeys {
    static let endpoint = "server.endpoint"
    static let enabledTypes = "types.enabled"
    static let tokenAccount = "server-token"
    static let outboxSequence = "outbox.seq"
}

/// User configuration. The @Observable instance drives the UI; the static
/// accessors read the same storage (UserDefaults / Keychain, both
/// thread-safe) so background actors never touch the UI object.
@Observable
@MainActor
final class AppSettings {
    var endpoint: String {
        didSet { UserDefaults.standard.set(endpoint, forKey: SettingsKeys.endpoint) }
    }

    var token: String {
        didSet { Keychain.set(token.isEmpty ? nil : token, for: SettingsKeys.tokenAccount) }
    }

    private var enabled: [String: Bool] {
        didSet { UserDefaults.standard.set(enabled, forKey: SettingsKeys.enabledTypes) }
    }

    init() {
        endpoint = UserDefaults.standard.string(forKey: SettingsKeys.endpoint) ?? ""
        token = Keychain.string(for: SettingsKeys.tokenAccount) ?? ""
        enabled = Self.storedEnabled()
    }

    func isEnabled(_ key: String) -> Bool { enabled[key] ?? true }
    func setEnabled(_ key: String, _ value: Bool) { enabled[key] = value }

    var baseURL: URL? { Self.baseURL(from: endpoint) }
    var healthURL: URL? { baseURL.flatMap { URL(string: $0.absoluteString + "/health") } }

    // MARK: - Shared parsing / thread-safe accessors

    nonisolated static func baseURL(from raw: String) -> URL? {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }
        if !s.contains("://") { s = "http://" + s }
        while s.hasSuffix("/") { s.removeLast() }
        guard let url = URL(string: s), url.host() != nil else { return nil }
        return url
    }

    /// Where to POST, resolved at send time. nil = not configured yet.
    nonisolated static func currentTarget() -> (url: URL, token: String?)? {
        let raw = UserDefaults.standard.string(forKey: SettingsKeys.endpoint) ?? ""
        guard let base = baseURL(from: raw),
              let url = URL(string: base.absoluteString + "/ingest") else { return nil }
        return (url, Keychain.string(for: SettingsKeys.tokenAccount))
    }

    nonisolated static func currentEnabled(_ key: String) -> Bool {
        storedEnabled()[key] ?? true
    }

    nonisolated static func currentEnabledTypes() -> [SyncedType] {
        TypeRegistry.all.filter { currentEnabled($0.key) }
    }

    nonisolated private static func storedEnabled() -> [String: Bool] {
        (UserDefaults.standard.dictionary(forKey: SettingsKeys.enabledTypes) ?? [:])
            .compactMapValues { $0 as? Bool }
    }
}
