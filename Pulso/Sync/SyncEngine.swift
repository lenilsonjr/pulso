import Foundation
import HealthKit

enum SyncReason: CustomStringConvertible, Sendable {
    case manual
    case foreground
    case observer(String)
    case backgroundRefresh
    case settingsChanged

    var description: String {
        switch self {
        case .manual: "manual"
        case .foreground: "app opened"
        case .observer(let key): "new \(key) data"
        case .backgroundRefresh: "background refresh"
        case .settingsChanged: "settings changed"
        }
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
    private var anchorCache: [String: HKQueryAnchor] = [:]
    private var chain: Task<Void, Never>?

    init(healthStore: HKHealthStore, anchors: AnchorStore, outbox: Outbox, log: LogStore, status: AppStatus) {
        self.healthStore = healthStore
        self.anchors = anchors
        self.outbox = outbox
        self.log = log
        self.status = status
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
        var queued = 0
        for type in AppSettings.currentEnabledTypes() {
            do {
                queued += try await sync(type)
            } catch {
                await log.error("\(type.key): \(error.localizedDescription)")
            }
        }
        let force: Bool
        switch reason {
        case .manual, .foreground, .settingsChanged: force = true
        case .observer, .backgroundRefresh: force = false
        }
        await outbox.drain(force: force)
        await status.setSyncing(false)
        if queued > 0 {
            await log.info("sync (\(reason)): queued \(queued) sample(s)")
        }
    }

    private func sync(_ type: SyncedType) async throws -> Int {
        var anchor = anchorCache[type.key] ?? anchors.load(type.key)
        var queued = 0
        while true {
            let descriptor = HKAnchoredObjectQueryDescriptor(
                predicates: [.sample(type: type.sampleType)],
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
