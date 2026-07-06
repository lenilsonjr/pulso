import Foundation
import HealthKit

/// Persists one HKQueryAnchor per type, one file each. The crash-safety
/// invariant of the whole app: an anchor is only written after the server
/// has ACKed every sample it covers (or when it covers no samples at all).
struct AnchorStore: Sendable {
    let directory: URL

    init(directory: URL) {
        self.directory = directory
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    private func url(for key: String) -> URL {
        directory.appendingPathComponent("\(key).anchor")
    }

    func load(_ key: String) -> HKQueryAnchor? {
        guard let data = try? Data(contentsOf: url(for: key)) else { return nil }
        return try? NSKeyedUnarchiver.unarchivedObject(ofClass: HKQueryAnchor.self, from: data)
    }

    func archive(_ anchor: HKQueryAnchor) -> Data? {
        try? NSKeyedArchiver.archivedData(withRootObject: anchor, requiringSecureCoding: true)
    }

    func save(_ anchor: HKQueryAnchor, for key: String) {
        guard let data = archive(anchor) else { return }
        saveRaw(data, for: key)
    }

    func saveRaw(_ data: Data, for key: String) {
        try? data.write(to: url(for: key), options: .atomic)
    }

    func hasAnchor(_ key: String) -> Bool {
        FileManager.default.fileExists(atPath: url(for: key).path)
    }

    func clearAll() {
        try? FileManager.default.removeItem(at: directory)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }
}
