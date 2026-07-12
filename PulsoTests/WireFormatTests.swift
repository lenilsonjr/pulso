import XCTest
@testable import Pulso

final class WireFormatTests: XCTestCase {
    private var sleepDTO: SampleDTO {
        SampleDTO(
            uuid: "ABC-123",
            type: "sleepAnalysis",
            start: "2026-07-06T01:12:00+01:00",
            end: "2026-07-06T02:40:00+01:00",
            value: .string("asleepREM"),
            source: "Apple Watch",
            sourceBundleId: "com.apple.health",
            metadata: ["timeZone": .string("Europe/Lisbon")]
        )
    }

    /// Golden test: this exact wire format is what docs/PROTOCOL.md promises.
    func testSampleGoldenEncoding() throws {
        let json = String(decoding: try Wire.encodeElements([.sample(sleepDTO)]), as: UTF8.self)
        XCTAssertEqual(json, """
        [{"end":"2026-07-06T02:40:00+01:00","metadata":{"timeZone":"Europe/Lisbon"},\
        "source":"Apple Watch","sourceBundleId":"com.apple.health",\
        "start":"2026-07-06T01:12:00+01:00","type":"sleepAnalysis","uuid":"ABC-123",\
        "value":"asleepREM"}]
        """)
    }

    func testNumberValueEncodesAsJSONNumber() throws {
        var dto = sleepDTO
        dto.value = .number(58)
        dto.unit = "count/min"
        let json = String(decoding: try Wire.encodeElements([.sample(dto)]), as: UTF8.self)
        XCTAssertTrue(json.contains(#""value":58"#), json)
        XCTAssertTrue(json.contains(#""unit":"count\/min""#) || json.contains(#""unit":"count/min""#), json)
    }

    /// Golden test for the v1.1 workout enrichment: full metadata (string /
    /// bool / number) and the statistics fields.
    func testWorkoutGoldenEncoding() throws {
        var dto = SampleDTO(
            uuid: "W-1",
            type: "workout",
            start: "2026-07-05T18:30:00+01:00",
            end: "2026-07-05T19:30:12+01:00",
            source: "SmartGym",
            metadata: [
                "HKIndoorWorkout": .bool(true),
                "HKWorkoutBrandName": .string("Push Day A"),
                "customRounds": .number(3),
            ]
        )
        dto.workoutActivityType = "traditionalStrengthTraining"
        dto.duration = 3612.4
        dto.totalEnergyBurned = 412
        dto.totalBasalEnergyBurned = 96.5
        dto.averageHeartRate = 132.5
        dto.minimumHeartRate = 88
        dto.maximumHeartRate = 171

        let json = String(decoding: try Wire.encodeElements([.sample(dto)]), as: UTF8.self)
        XCTAssertEqual(json, """
        [{"averageHeartRate":132.5,"duration":3612.4,"end":"2026-07-05T19:30:12+01:00",\
        "maximumHeartRate":171,"metadata":{"HKIndoorWorkout":true,"HKWorkoutBrandName":"Push Day A",\
        "customRounds":3},"minimumHeartRate":88,"source":"SmartGym",\
        "start":"2026-07-05T18:30:00+01:00","totalBasalEnergyBurned":96.5,"totalEnergyBurned":412,\
        "type":"workout","uuid":"W-1","workoutActivityType":"traditionalStrengthTraining"}]
        """)
    }

    func testJSONValueDecodingRoundTrip() throws {
        let original: [String: JSONValue] = [
            "s": .string("x"), "n": .number(2.5), "whole": .number(3), "b": .bool(true), "f": .bool(false),
        ]
        let data = try JSONEncoder().encode(original)
        let decoded = try JSONDecoder().decode([String: JSONValue].self, from: data)
        XCTAssertEqual(decoded, original)
    }

    func testTombstoneEncoding() throws {
        let json = String(decoding: try Wire.encodeElements([.tombstone(["U1", "U2"])]), as: UTF8.self)
        XCTAssertEqual(json, #"[{"deleted":["U1","U2"]}]"#)
    }

    func testBatchElementDecodingRoundTrip() throws {
        let elements: [BatchElement] = [.sample(sleepDTO), .tombstone(["U1"])]
        let data = try Wire.encodeElements(elements)
        let decoded = try JSONDecoder().decode([BatchElement].self, from: data)
        XCTAssertEqual(decoded, elements)
    }

    func testBatchFileRoundTrip() throws {
        let batch = Batch(
            id: UUID(),
            typeKey: "sleepAnalysis",
            createdAt: Date(timeIntervalSince1970: 1_780_000_000),
            anchor: Data([1, 2, 3]),
            elements: [.sample(sleepDTO)]
        )
        let decoded = try Wire.fileDecoder().decode(Batch.self, from: Wire.fileEncoder().encode(batch))
        XCTAssertEqual(decoded.id, batch.id)
        XCTAssertEqual(decoded.typeKey, "sleepAnalysis")
        XCTAssertEqual(decoded.anchor, Data([1, 2, 3]))
        XCTAssertEqual(decoded.elements, batch.elements)
        XCTAssertEqual(decoded.sampleCount, 1)
    }

    func testSlicerSplitsBySizeAndPreservesOrder() throws {
        let elements: [BatchElement] = (0 ..< 16).map { i in
            var dto = sleepDTO
            dto.uuid = "id-\(i)-" + String(repeating: "x", count: 200)
            return .sample(dto)
        }
        let whole = try Wire.encodeElements(elements)
        let slices = try BatchSlicer.slice(elements, maxBytes: whole.count / 4)
        XCTAssertGreaterThan(slices.count, 1)
        XCTAssertEqual(slices.flatMap { $0 }, elements, "no element lost or reordered")
        for slice in slices where slice.count > 1 {
            XCTAssertLessThanOrEqual(try Wire.encodeElements(slice).count, whole.count / 4)
        }
    }

    func testSlicerLeavesSmallBatchesAlone() throws {
        let elements: [BatchElement] = [.sample(sleepDTO), .tombstone(["U1"])]
        let slices = try BatchSlicer.slice(elements, maxBytes: Wire.maxBatchBytes)
        XCTAssertEqual(slices, [elements])
    }

    func testBaseURLParsing() {
        XCTAssertEqual(
            AppSettings.baseURL(from: "100.64.0.7:8787")?.absoluteString,
            "http://100.64.0.7:8787"
        )
        XCTAssertEqual(
            AppSettings.baseURL(from: " http://100.64.0.7:8787/ ")?.absoluteString,
            "http://100.64.0.7:8787"
        )
        XCTAssertEqual(
            AppSettings.baseURL(from: "https://pulso.example.ts.net")?.absoluteString,
            "https://pulso.example.ts.net"
        )
        XCTAssertNil(AppSettings.baseURL(from: ""))
        XCTAssertNil(AppSettings.baseURL(from: "   "))
        XCTAssertNil(AppSettings.baseURL(from: "http://"))
    }
}
