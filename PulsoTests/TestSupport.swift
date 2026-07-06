import Compression
import Foundation
@testable import Pulso

enum TestSupport {
    /// Inverse of Gzip.compress's DEFLATE payload, for round-trip assertions.
    static func inflate(_ data: Data, expectedSize: Int) -> Data {
        guard expectedSize > 0 else { return Data() }
        let destination = UnsafeMutablePointer<UInt8>.allocate(capacity: expectedSize)
        defer { destination.deallocate() }
        let written = data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Int in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return 0 }
            return compression_decode_buffer(destination, expectedSize, base, data.count, nil, COMPRESSION_ZLIB)
        }
        return Data(bytes: destination, count: written)
    }

    /// Strips the gzip framing and inflates, so tests can inspect upload bodies.
    static func gunzip(_ data: Data, expectedSize: Int) -> Data {
        inflate(Data(data.dropFirst(10).dropLast(8)), expectedSize: expectedSize)
    }

    static func date(
        _ year: Int, _ month: Int, _ day: Int, _ hour: Int, _ minute: Int,
        zone: String
    ) -> Date {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: zone)!
        return calendar.date(from: DateComponents(year: year, month: month, day: day, hour: hour, minute: minute))!
    }
}
