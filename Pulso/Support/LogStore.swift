import Foundation
import os

struct LogEntry: Codable, Identifiable, Equatable, Sendable {
    enum Level: String, Codable, Sendable { case info, warn, error }

    var id = UUID()
    let date: Date
    let level: Level
    let message: String
}

/// Append-only NDJSON activity log, persisted so background sync activity is
/// visible the next time the app is opened. Mirrors to os.Logger for Console.
actor LogStore {
    private let fileURL: URL
    private let maxBytes = 512 * 1024
    private let logger = Logger(subsystem: Bundle.main.bundleIdentifier ?? "pulso", category: "sync")
    private var sink: (@Sendable (LogEntry) -> Void)?

    init(directory: URL) {
        fileURL = directory.appendingPathComponent("pulso-log.ndjson")
    }

    /// Receives every new entry (used to feed the UI on the main actor).
    func setSink(_ sink: @escaping @Sendable (LogEntry) -> Void) {
        self.sink = sink
    }

    func info(_ message: String) { append(.info, message) }
    func warn(_ message: String) { append(.warn, message) }
    func error(_ message: String) { append(.error, message) }

    private func append(_ level: LogEntry.Level, _ message: String) {
        let entry = LogEntry(date: Date(), level: level, message: message)
        switch level {
        case .info: logger.info("\(message, privacy: .public)")
        case .warn: logger.warning("\(message, privacy: .public)")
        case .error: logger.error("\(message, privacy: .public)")
        }
        persist(entry)
        sink?(entry)
    }

    private func persist(_ entry: LogEntry) {
        guard var line = try? Self.encoder().encode(entry) else { return }
        line.append(0x0A)
        let fm = FileManager.default
        if !fm.fileExists(atPath: fileURL.path) {
            try? line.write(to: fileURL, options: .atomic)
            return
        }
        guard let handle = try? FileHandle(forWritingTo: fileURL) else { return }
        defer { try? handle.close() }
        do {
            try handle.seekToEnd()
            try handle.write(contentsOf: line)
        } catch { return }
        if (try? handle.offset()).map({ $0 > UInt64(maxBytes) }) == true {
            rotate()
        }
    }

    /// Keeps the newest half of the file, cut at a line boundary.
    private func rotate() {
        guard let data = try? Data(contentsOf: fileURL) else { return }
        let tail = data.suffix(maxBytes / 2)
        guard let newline = tail.firstIndex(of: 0x0A) else { return }
        try? data[data.index(after: newline)...].write(to: fileURL, options: .atomic)
    }

    func tail(_ count: Int) -> [LogEntry] {
        guard let data = try? Data(contentsOf: fileURL) else { return [] }
        let decoder = Self.decoder()
        let lines = data.split(separator: 0x0A)
        return lines.suffix(count).compactMap { try? decoder.decode(LogEntry.self, from: $0) }
    }

    private static func encoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        return e
    }

    private static func decoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .iso8601
        return d
    }
}
