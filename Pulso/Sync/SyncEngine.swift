import Foundation
import HealthKit

enum SyncReason: CustomStringConvertible, Sendable {
    case manual
    case foreground
    case observer(String)
    case backgroundRefresh
    case settingsChanged
    case reimport

    var description: String {
        switch self {
        case .manual: "manual"
        case .foreground: "app opened"
        case .observer(let key): "new \(key) data"
        case .backgroundRefresh: "background refresh"
        case .settingsChanged: "settings changed"
        case .reimport: "full re-import"
        }
    }
}

enum CatchUp {
    /// Safety margin subtracted from the server's newest timestamp: sources
    /// like WHOOP backfill days late, and re-sent overlap is free (the
    /// server dedupes by uuid).
    static let margin: TimeInterval = 72 * 3600

    static func cutoffs(fromLatest latest: [String: String]) -> [String: Date] {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return latest.compactMapValues { formatter.date(from: $0)?.addingTimeInterval(-margin) }
    }
}

/// Per enabled type: run an anchored query from the last anchor, hand the
/// results to the outbox, and let the outbox persist the new anchor once the
/// server ACKs. An in-memory anchor cache leads the persisted one so
/// overlapping triggers don't re-read samples that are already queued; after
/// a crash the cache is gone, the persisted anchor wins, and any re-read
/// samples dedupe server-side by uuid.
actor SyncEngine {
    static let queryLimit = 5000

    private let healthStore: HKHealthStore
    private let anchors: AnchorStore
    private let outbox: Outbox
    private let log: LogStore
    private let status: AppStatus
    /// GET /latest from the configured server; nil when unavailable.
    private let latestHints: @Sendable () async -> [String: String]?
    private var anchorCache: [String: HKQueryAnchor] = [:]
    private var chain: Task<Void, Never>?

    init(
        healthStore: HKHealthStore, anchors: AnchorStore, outbox: Outbox,
        log: LogStore, status: AppStatus,
        latestHints: @escaping @Sendable () async -> [String: String]? = { nil }
    ) {
        self.healthStore = healthStore
        self.anchors = anchors
        self.outbox = outbox
        self.log = log
        self.status = status
        self.latestHints = latestHints
    }

    /// Every trigger funnels here. Passes are serialized; each caller returns
    /// after its own pass (and the outbox drain it triggers) completes.
    func requestSync(_ reason: SyncReason) async {
        let previous = chain
        let task = Task {
            await previous?.value
            await self.performSync(reason)
        }
        chain = task
        await task.value
    }

    private func performSync(_ reason: SyncReason) async {
        guard AppSettings.currentTarget() != nil else {
            await log.info("sync skipped (\(reason)): no server configured")
            return
        }
        await status.setSyncing(true)
        let enabled = AppSettings.currentEnabledTypes()

        // Catch-up: for types with no anchor (fresh install, newly enabled),
        // ask the server what it already has and skip re-reading history.
        // A full re-import must bypass this, or it couldn't re-send.
        var cutoffs: [String: Date] = [:]
        if case .reimport = reason {
            // no cutoffs — read everything
        } else if enabled.contains(where: { anchorCache[$0.key] == nil && !anchors.hasAnchor($0.key) }) {
            if let latest = await latestHints() {
                cutoffs = CatchUp.cutoffs(fromLatest: latest)
                if !cutoffs.isEmpty {
                    await log.info("server /latest: catching up instead of full backfill for \(cutoffs.count) type(s)")
                }
            }
        }

        var queued = 0
        for type in enabled {
            do {
                queued += try await sync(type, cutoffs: cutoffs)
            } catch {
                await log.error("\(type.key): \(error.localizedDescription)")
            }
        }
        let force: Bool
        switch reason {
        case .manual, .foreground, .settingsChanged, .reimport: force = true
        case .observer, .backgroundRefresh: force = false
        }
        await outbox.drain(force: force)
        await status.setSyncing(false)
        if queued > 0 {
            await log.info("sync (\(reason)): queued \(queued) sample(s)")
        }
    }

    private func sync(_ type: SyncedType, cutoffs: [String: Date] = [:]) async throws -> Int {
        var anchor = anchorCache[type.key] ?? anchors.load(type.key)
        // Date-bounding a nil-anchor query skips the historical read while
        // still returning a store-wide anchor, so later incremental syncs
        // (which drop the predicate) see every subsequent change.
        var datePredicate: NSPredicate?
        if anchor == nil, let cutoff = cutoffs[type.key] {
            datePredicate = HKQuery.predicateForSamples(withStart: cutoff, end: nil)
            await log.info("\(type.key): server has history — backfilling only from \(cutoff.formatted(.iso8601))")
        }
        var queued = 0
        while true {
            let descriptor = HKAnchoredObjectQueryDescriptor(
                predicates: [.sample(type: type.sampleType, predicate: datePredicate)],
                anchor: anchor,
                limit: Self.queryLimit
            )
            let result = try await descriptor.result(for: healthStore)
            let added = result.addedSamples
            let deleted = result.deletedObjects.map { $0.uuid.uuidString }
            let newAnchor = normalize(result.newAnchor)

            if added.isEmpty && deleted.isEmpty {
                // Nothing new — the anchor covers no undelivered samples, so
                // persisting it immediately is safe.
                if let newAnchor {
                    anchorCache[type.key] = newAnchor
                    anchors.save(newAnchor, for: type.key)
                }
                await status.recordChecked(type.key)
                return queued
            }

            let dtos = added.compactMap { type.serialize($0, .live) }
            if dtos.count < added.count {
                await log.warn("\(type.key): \(added.count - dtos.count) sample(s) not serializable — skipped")
            }
            var elements = dtos.map(BatchElement.sample)
            if !deleted.isEmpty {
                elements.append(.tombstone(deleted))
            }
            let anchorData = newAnchor.flatMap { anchors.archive($0) }
            try await outbox.enqueue(typeKey: type.key, elements: elements, anchor: anchorData)
            if let newAnchor {
                anchorCache[type.key] = newAnchor
                anchor = newAnchor
            }
            queued += dtos.count
            await status.recordChecked(type.key)
            if !deleted.isEmpty {
                await log.info("\(type.key): queued \(dtos.count) sample(s) + \(deleted.count) deletion(s)")
            } else {
                await log.info("\(type.key): queued \(dtos.count) sample(s)")
            }
            if added.count < Self.queryLimit && deleted.count < Self.queryLimit {
                return queued
            }
        }
    }

    /// The SDK has flip-flopped on the optionality of `newAnchor`; funneling
    /// it through an optional parameter compiles either way.
    private func normalize(_ anchor: HKQueryAnchor?) -> HKQueryAnchor? {
        anchor
    }

    func resetAllAnchors() {
        anchorCache = [:]
    }
}
