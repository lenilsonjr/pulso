import XCTest
@testable import Pulso

/// Scriptable uploader. Unscripted calls succeed with HTTP 200.
final class MockUploader: Uploading, @unchecked Sendable {
    struct Call {
        let body: Data
        let url: URL
        let token: String?
    }

    static let ok = UploadResult(status: 200, bodySnippet: #"{"received":1}"#, duration: 0.01)

    private let lock = NSLock()
    private var script: [Result<UploadResult, Error>] = []
    private var calls: [Call] = []

    func enqueueResult(_ result: Result<UploadResult, Error>) {
        lock.lock()
        script.append(result)
        lock.unlock()
    }

    var callCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return calls.count
    }

    func call(_ index: Int) -> Call {
        lock.lock()
        defer { lock.unlock() }
        return calls[index]
    }

    func send(_ body: Data, gzipped: Bool, to url: URL, token: String?) async throws -> UploadResult {
        switch record(Call(body: body, url: url, token: token)) {
        case .none:
            return Self.ok
        case .success(let result):
            return result
        case .failure(let error):
            throw error
        }
    }

    private func record(_ call: Call) -> Result<UploadResult, Error>? {
        lock.lock()
        defer { lock.unlock() }
        calls.append(call)
        return script.isEmpty ? nil : script.removeFirst()
    }
}

@MainActor
final class OutboxTests: XCTestCase {
    private var base: URL!
    private var outboxDir: URL!
    private var anchors: AnchorStore!
    private var uploader: MockUploader!
    private var status: AppStatus!
    private var log: LogStore!

    override func setUp() async throws {
        base = FileManager.default.temporaryDirectory
            .appendingPathComponent("outbox-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        outboxDir = base.appendingPathComponent("Outbox")
        anchors = AnchorStore(directory: base.appendingPathComponent("Anchors"))
        uploader = MockUploader()
        status = AppStatus()
        log = LogStore(directory: base)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: base)
    }

    private func makeOutbox(maxBatchBytes: Int = Wire.maxBatchBytes, hasTarget: Bool = true) -> Outbox {
        let target: @Sendable () -> (url: URL, token: String?)?
        if hasTarget {
            let url = URL(string: "http://127.0.0.1:1/ingest")!
            target = { (url: url, token: "tok") }
        } else {
            target = { nil }
        }
        return Outbox(
            directory: outboxDir,
            anchors: anchors,
            uploader: uploader,
            target: target,
            log: log,
            status: status,
            maxBatchBytes: maxBatchBytes
        )
    }

    private func element(_ id: String, pad: Int = 0) -> BatchElement {
        .sample(SampleDTO(
            uuid: id + String(repeating: "x", count: pad),
            type: "sleepAnalysis",
            start: "2026-07-06T01:12:00+01:00",
            end: "2026-07-06T02:40:00+01:00",
            value: .string("inBed"),
            source: "test"
        ))
    }

    private func anchorData(_ key: String) -> Data? {
        try? Data(contentsOf: base.appendingPathComponent("Anchors/\(key).anchor"))
    }

    // MARK: - Tests

    func testDrainDeliversFIFOPersistsAnchorsAndClearsFiles() async throws {
        let outbox = makeOutbox()
        let anchorA1 = Data("anchor-a1".utf8)
        let anchorA2 = Data("anchor-a2".utf8)
        let anchorB1 = Data("anchor-b1".utf8)

        try await outbox.enqueue(typeKey: "a", elements: [element("first")], anchor: anchorA1)
        try await outbox.enqueue(typeKey: "a", elements: [element("second")], anchor: anchorA2)
        try await outbox.enqueue(typeKey: "b", elements: [element("third")], anchor: anchorB1)
        await outbox.drain(force: true)

        XCTAssertEqual(uploader.callCount, 3)
        let pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 0)

        // The anchor moves only when the server says yes — and in order.
        XCTAssertEqual(anchorData("a"), anchorA2)
        XCTAssertEqual(anchorData("b"), anchorB1)

        // FIFO: bodies arrive in enqueue order.
        let firstBody = TestSupport.gunzip(uploader.call(0).body, expectedSize: 1 << 20)
        let decoded = try JSONDecoder().decode([BatchElement].self, from: firstBody)
        guard case .sample(let dto) = decoded[0] else { return XCTFail("expected a sample") }
        XCTAssertTrue(dto.uuid.hasPrefix("first"))
        XCTAssertEqual(uploader.call(0).token, "tok")
        XCTAssertEqual(uploader.call(0).url.path, "/ingest")
    }

    func testFailureStopsDrainKeepsFilesAndBacksOff() async throws {
        uploader.enqueueResult(.failure(URLError(.cannotConnectToHost)))
        let outbox = makeOutbox()

        try await outbox.enqueue(typeKey: "a", elements: [element("one")], anchor: Data("a1".utf8))
        try await outbox.enqueue(typeKey: "a", elements: [element("two")], anchor: Data("a2".utf8))
        await outbox.drain(force: false) // barrier for the enqueue-spawned drains

        // First attempt failed; backoff must swallow non-forced retries.
        XCTAssertEqual(uploader.callCount, 1)
        var pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 2)
        XCTAssertNil(anchorData("a"), "no ACK, no anchor")

        await outbox.drain(force: false)
        XCTAssertEqual(uploader.callCount, 1, "backoff window must suppress retries")

        // Manual sync bypasses the backoff.
        await outbox.drain(force: true)
        XCTAssertEqual(uploader.callCount, 3)
        pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 0)
        XCTAssertEqual(anchorData("a"), Data("a2".utf8))
    }

    func testNon2xxResponseIsAFailure() async throws {
        uploader.enqueueResult(.success(UploadResult(status: 500, bodySnippet: "boom", duration: 0.01)))
        let outbox = makeOutbox()

        try await outbox.enqueue(typeKey: "a", elements: [element("one")], anchor: Data("a1".utf8))
        await outbox.drain(force: false)

        let pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 1)
        XCTAssertNil(anchorData("a"))
    }

    func testCorruptBatchFileIsQuarantinedNotRetriedForever() async throws {
        let outbox = makeOutbox()
        try FileManager.default.createDirectory(at: outboxDir, withIntermediateDirectories: true)
        try Data("this is not json".utf8)
            .write(to: outboxDir.appendingPathComponent("batch-000000000001-dead.json"))

        await outbox.drain(force: true)

        XCTAssertEqual(uploader.callCount, 0)
        let pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 0)
        let names = try FileManager.default.contentsOfDirectory(atPath: outboxDir.path)
        XCTAssertEqual(names, ["batch-000000000001-dead.json.corrupt"])
    }

    func testOversizedEnqueueSplitsAndOnlyLastSliceCarriesAnchor() async throws {
        // No server configured: files stay on disk for inspection.
        let outbox = makeOutbox(maxBatchBytes: 700, hasTarget: false)
        let anchor = Data("final-anchor".utf8)

        try await outbox.enqueue(
            typeKey: "a",
            elements: (0 ..< 8).map { element("id-\($0)", pad: 150) },
            anchor: anchor
        )
        await outbox.drain(force: true) // no-op without a target; barrier only

        let files = try FileManager.default.contentsOfDirectory(at: outboxDir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
        XCTAssertGreaterThan(files.count, 1, "batch should have been split")

        var totalSamples = 0
        for (index, file) in files.enumerated() {
            let batch = try Wire.fileDecoder().decode(Batch.self, from: Data(contentsOf: file))
            totalSamples += batch.sampleCount
            if index == files.count - 1 {
                XCTAssertEqual(batch.anchor, anchor, "only the last slice may advance the anchor")
            } else {
                XCTAssertNil(batch.anchor)
            }
        }
        XCTAssertEqual(totalSamples, 8)
        XCTAssertEqual(uploader.callCount, 0)
    }

    func testRemoveAllClearsQueue() async throws {
        let outbox = makeOutbox(hasTarget: false)
        try await outbox.enqueue(typeKey: "a", elements: [element("one")], anchor: nil)
        var pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 1)

        await outbox.removeAll()
        pending = await outbox.pendingCount()
        XCTAssertEqual(pending, 0)
    }

    func testUploadBodyIsGzippedWireFormat() async throws {
        let outbox = makeOutbox()
        let elements: [BatchElement] = [element("one"), .tombstone(["gone-1", "gone-2"])]
        try await outbox.enqueue(typeKey: "a", elements: elements, anchor: nil)
        await outbox.drain(force: true)

        XCTAssertEqual(uploader.callCount, 1)
        let body = uploader.call(0).body
        XCTAssertEqual(Array(body.prefix(2)), [0x1f, 0x8b], "body must be gzipped")
        let decoded = try JSONDecoder().decode(
            [BatchElement].self,
            from: TestSupport.gunzip(body, expectedSize: 1 << 20)
        )
        XCTAssertEqual(decoded, elements)
    }
}
