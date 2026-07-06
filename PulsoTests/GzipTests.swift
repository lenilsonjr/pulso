import XCTest
@testable import Pulso

final class GzipTests: XCTestCase {
    func testCRC32KnownVectors() {
        // The standard CRC-32 check value.
        XCTAssertEqual(Gzip.crc32(Data("123456789".utf8)), 0xCBF4_3926)
        XCTAssertEqual(Gzip.crc32(Data()), 0)
    }

    func testHeaderTrailerAndRoundTrip() {
        let payload = Data(String(repeating: "pulso wire format ", count: 500).utf8)
        let gz = Gzip.compress(payload)

        XCTAssertEqual(Array(gz.prefix(4)), [0x1f, 0x8b, 0x08, 0x00])
        XCTAssertLessThan(gz.count, payload.count, "text should compress")

        let trailer = Array(gz.suffix(8))
        let crc = trailer[0 ..< 4].enumerated().reduce(UInt32(0)) { $0 | (UInt32($1.element) << (8 * $1.offset)) }
        let isize = trailer[4 ..< 8].enumerated().reduce(UInt32(0)) { $0 | (UInt32($1.element) << (8 * $1.offset)) }
        XCTAssertEqual(crc, Gzip.crc32(payload))
        XCTAssertEqual(isize, UInt32(payload.count))

        XCTAssertEqual(TestSupport.gunzip(gz, expectedSize: payload.count), payload)
    }

    func testEmptyInput() {
        let gz = Gzip.compress(Data())
        XCTAssertEqual(gz.count, 20) // 10 header + 2 empty deflate block + 8 trailer
        XCTAssertEqual(Array(gz.suffix(8)), [0, 0, 0, 0, 0, 0, 0, 0])
    }

    func testIncompressibleInputSurvives() {
        var noise = Data((0 ..< 100_000).map { _ in UInt8.random(in: 0 ... 255) })
        noise.append(Data("tail".utf8))
        let gz = Gzip.compress(noise)
        XCTAssertEqual(TestSupport.gunzip(gz, expectedSize: noise.count), noise)
    }
}
