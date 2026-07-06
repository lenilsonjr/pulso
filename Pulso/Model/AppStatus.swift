import Foundation
import Observation

struct TypeStatus: Codable, Equatable, Sendable {
    var lastChecked: Date?
    var lastDelivered: Date?
    var totalDelivered: Int = 0
}

/// Observable UI state. All mutation happens on the main actor; background
/// actors call the methods below with await.
@Observable
@MainActor
final class AppStatus {
    var healthUnavailable = false
    var authNeeded = false
    var syncing = false
    var needsInitialImport = false
    var typeStatus: [String: TypeStatus] = [:]
    var outboxPending = 0
    var outboxNextRetry: Date?
    var lastResponse: String?
    var recentLog: [LogEntry] = []

    private static let statusKey = "status.types"
    private static let responseKey = "status.lastResponse"
    private let maxLogEntries = 200

    func load() {
        if let data = UserDefaults.standard.data(forKey: Self.statusKey),
           let decoded = try? JSONDecoder().decode([String: TypeStatus].self, from: data) {
            typeStatus = decoded
        }
        lastResponse = UserDefaults.standard.string(forKey: Self.responseKey)
    }

    private func save() {
        if let data = try? JSONEncoder().encode(typeStatus) {
            UserDefaults.standard.set(data, forKey: Self.statusKey)
        }
        UserDefaults.standard.set(lastResponse, forKey: Self.responseKey)
    }

    func setSyncing(_ value: Bool) {
        syncing = value
    }

    func recordChecked(_ key: String) {
        update(key) { $0.lastChecked = Date() }
    }

    func recordDelivery(typeKey: String, count: Int, response: String) {
        update(typeKey) {
            $0.lastDelivered = Date()
            $0.totalDelivered += count
        }
        lastResponse = response
        save()
    }

    func setLastResponse(_ response: String) {
        lastResponse = response
        save()
    }

    func setOutbox(pending: Int, nextRetry: Date?) {
        outboxPending = pending
        outboxNextRetry = nextRetry
    }

    func resetCounters() {
        typeStatus = [:]
        lastResponse = nil
        save()
    }

    func push(_ entry: LogEntry) {
        recentLog.insert(entry, at: 0)
        if recentLog.count > maxLogEntries {
            recentLog.removeLast(recentLog.count - maxLogEntries)
        }
    }

    func seedLog(_ entries: [LogEntry]) {
        recentLog = entries.sorted { $0.date > $1.date }
    }

    private func update(_ key: String, _ mutate: (inout TypeStatus) -> Void) {
        var status = typeStatus[key] ?? TypeStatus()
        mutate(&status)
        typeStatus[key] = status
        save()
    }
}
