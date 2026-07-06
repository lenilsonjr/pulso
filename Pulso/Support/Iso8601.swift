import Foundation

/// Formats timestamps as ISO 8601 *with the sample's own UTC offset* — never
/// normalized to UTC. Apple's own export.xml loses original timezones this
/// way; Pulso deliberately does not. See docs/PROTOCOL.md.
final class TimestampFormatter: @unchecked Sendable {
    private var cache: [String: ISO8601DateFormatter] = [:]
    private let lock = NSLock()

    func string(_ date: Date, in timeZone: TimeZone) -> String {
        lock.lock()
        defer { lock.unlock() }
        let formatter: ISO8601DateFormatter
        if let cached = cache[timeZone.identifier] {
            formatter = cached
        } else {
            formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime]
            formatter.timeZone = timeZone
            cache[timeZone.identifier] = formatter
        }
        return formatter.string(from: date)
    }
}
