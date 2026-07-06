import Foundation

/// Durable delivery queue. Every batch is written to disk *before* the first
/// upload attempt and deleted only on a server 2xx — at which point the
/// batch's anchor (if any) is persisted. Files drain strictly in enqueue
/// order and draining stops at the first failure, so anchors can never
/// advance past an undelivered batch. At-least-once delivery; the server
/// dedupes by sample uuid.
actor Outbox {
    static let baseRetryInterval: TimeInterval = 60
    static let maxRetryInterval: TimeInterval = 3600

    private let directory: URL
    private let anchors: AnchorStore
    private let uploader: Uploading
    private let target: @Sendable () -> (url: URL, token: String?)?
    private let log: LogStore
    private let status: AppStatus
    private let maxBatchBytes: Int
    private var sequence: Int
    private var failureCount = 0
    private var nextAttempt = Date.distantPast
    private var chain: Task<Void, Never>?

    init(
        directory: URL,
        anchors: AnchorStore,
        uploader: Uploading,
        target: @escaping @Sendable () -> (url: URL, token: String?)?,
        log: LogStore,
        status: AppStatus,
        maxBatchBytes: Int = Wire.maxBatchBytes
    ) {
        self.directory = directory
        self.anchors = anchors
        self.uploader = uploader
        self.target = target
        self.log = log
        self.status = status
        self.maxBatchBytes = maxBatchBytes
        sequence = UserDefaults.standard.integer(forKey: SettingsKeys.outboxSequence)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    // MARK: - Enqueue

    func enqueue(typeKey: String, elements: [BatchElement], anchor: Data?) throws {
        guard !elements.isEmpty else { return }
        let slices = try BatchSlicer.slice(elements, maxBytes: maxBatchBytes)
        for (index, slice) in slices.enumerated() {
            let batch = Batch(
                id: UUID(),
                typeKey: typeKey,
                createdAt: Date(),
                anchor: index == slices.count - 1 ? anchor : nil,
                elements: slice
            )
            let data = try Wire.fileEncoder().encode(batch)
            sequence += 1
            UserDefaults.standard.set(sequence, forKey: SettingsKeys.outboxSequence)
            let name = String(format: "batch-%012d-%@.json", sequence, String(batch.id.uuidString.prefix(8)))
            try data.write(to: directory.appendingPathComponent(name), options: .atomic)
        }
        // Pipeline uploads with whatever the caller does next.
        Task { await self.drain(force: false) }
    }

    // MARK: - Drain

    /// Serialized via task-chaining: concurrent calls queue up rather than
    /// interleave, and every caller returns only after its own pass finished.
    func drain(force: Bool) async {
        let previous = chain
        let task = Task {
            await previous?.value
            await self.performDrain(force: force)
        }
        chain = task
        await task.value
    }

    private func performDrain(force: Bool) async {
        defer { Task { await self.refreshPendingStatus() } }
        var files = pendingFiles()
        guard !files.isEmpty else { return }
        guard let target = target() else {
            await log.info("outbox: \(files.count) batch(es) waiting — no server configured")
            return
        }
        if !force && Date() < nextAttempt { return }

        while let file = files.first, !Task.isCancelled {
            files.removeFirst()
            let batch: Batch
            do {
                batch = try Wire.fileDecoder().decode(Batch.self, from: Data(contentsOf: file))
            } catch {
                // A batch file we can't read would poison the queue forever; set it aside.
                quarantine(file)
                await log.error("outbox: unreadable batch \(file.lastPathComponent) set aside (\(error.localizedDescription))")
                continue
            }
            do {
                let body = try Wire.encodeElements(batch.elements)
                let result = try await uploader.send(Gzip.compress(body), gzipped: true, to: target.url, token: target.token)
                guard result.isSuccess else {
                    await recordFailure("HTTP \(result.status) \(result.bodySnippet.prefix(120))", status: result.status)
                    return
                }
                try? FileManager.default.removeItem(at: file)
                if let anchorData = batch.anchor {
                    anchors.saveRaw(anchorData, for: batch.typeKey)
                }
                failureCount = 0
                nextAttempt = .distantPast
                let response = "HTTP \(result.status) · \(result.bodySnippet.prefix(80))"
                await status.recordDelivery(typeKey: batch.typeKey, count: batch.sampleCount, response: response)
                await log.info(
                    "delivered \(batch.sampleCount) \(batch.typeKey) sample(s) — HTTP \(result.status) in \(Int(result.duration * 1000))ms"
                )
            } catch {
                await recordFailure(error.localizedDescription, status: nil)
                return
            }
        }
    }

    private func recordFailure(_ reason: String, status statusCode: Int?) async {
        failureCount += 1
        let delay = min(
            Self.maxRetryInterval,
            Self.baseRetryInterval * pow(2, Double(failureCount - 1))
        )
        nextAttempt = Date(timeIntervalSinceNow: delay)
        var hint = ""
        if statusCode == 401 { hint = " — check the bearer token in Settings" }
        await status.setLastResponse(String(reason.prefix(160)))
        await log.warn("delivery failed: \(reason)\(hint) — retrying in \(Int(delay))s")
    }

    private func quarantine(_ file: URL) {
        let destination = file.appendingPathExtension("corrupt")
        try? FileManager.default.moveItem(at: file, to: destination)
    }

    // MARK: - State

    private func pendingFiles() -> [URL] {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: nil
        )) ?? []
        return contents
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    func pendingCount() -> Int {
        pendingFiles().count
    }

    func refreshPendingStatus() async {
        let count = pendingFiles().count
        await status.setOutbox(pending: count, nextRetry: failureCount > 0 && count > 0 ? nextAttempt : nil)
    }

    func removeAll() async {
        for file in pendingFiles() {
            try? FileManager.default.removeItem(at: file)
        }
        failureCount = 0
        nextAttempt = .distantPast
        await refreshPendingStatus()
    }
}
