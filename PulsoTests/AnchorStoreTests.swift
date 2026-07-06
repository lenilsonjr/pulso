import HealthKit
import XCTest
@testable import Pulso

final class AnchorStoreTests: XCTestCase {
    private var directory: URL!
    private var store: AnchorStore!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("anchors-\(UUID().uuidString)")
        store = AnchorStore(directory: directory)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    func testSaveAndLoadRoundTrip() throws {
        XCTAssertFalse(store.hasAnchor("sleepAnalysis"))
        XCTAssertNil(store.load("sleepAnalysis"))

        let anchor = HKQueryAnchor(fromValue: 42)
        store.save(anchor, for: "sleepAnalysis")

        XCTAssertTrue(store.hasAnchor("sleepAnalysis"))
        let loaded = try XCTUnwrap(store.load("sleepAnalysis"))
        // Round-tripping the loaded anchor must be byte-stable.
        XCTAssertEqual(store.archive(loaded), store.archive(anchor))
    }

    func testSaveRawMatchesWhatOutboxWrites() throws {
        let anchor = HKQueryAnchor(fromValue: 7)
        let data = try XCTUnwrap(store.archive(anchor))
        store.saveRaw(data, for: "workout")
        XCTAssertNotNil(store.load("workout"))
    }

    func testAnchorsAreIndependentPerType() {
        store.save(HKQueryAnchor(fromValue: 1), for: "a")
        XCTAssertTrue(store.hasAnchor("a"))
        XCTAssertFalse(store.hasAnchor("b"))
    }

    func testClearAll() {
        store.save(HKQueryAnchor(fromValue: 1), for: "a")
        store.save(HKQueryAnchor(fromValue: 2), for: "b")
        store.clearAll()
        XCTAssertFalse(store.hasAnchor("a"))
        XCTAssertFalse(store.hasAnchor("b"))
        // The directory must still be writable after a clear.
        store.save(HKQueryAnchor(fromValue: 3), for: "a")
        XCTAssertTrue(store.hasAnchor("a"))
    }
}
