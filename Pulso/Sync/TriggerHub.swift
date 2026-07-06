import BackgroundTasks
import HealthKit
import UIKit

/// Registers every sanctioned wake mechanism and funnels them all into
/// SyncEngine.requestSync: HealthKit observer queries with background
/// delivery (primary), BGAppRefreshTask (opportunistic backstop), and the
/// foreground hook (wired via AppServices.scenePhase).
@MainActor
final class TriggerHub {
    static let refreshTaskID = (Bundle.main.bundleIdentifier ?? "com.lenilson.pulso") + ".refresh"

    private let healthStore: HKHealthStore
    private let engine: SyncEngine
    private let log: LogStore
    private var observerQueries: [HKObserverQuery] = []

    init(healthStore: HKHealthStore, engine: SyncEngine, log: LogStore) {
        self.healthStore = healthStore
        self.engine = engine
        self.log = log
    }

    // MARK: - BGAppRefreshTask

    /// Must run before application(_:didFinishLaunchingWithOptions:) returns.
    func registerBackgroundRefresh() {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.refreshTaskID, using: nil) { [weak self] task in
            guard let refresh = task as? BGAppRefreshTask else {
                task.setTaskCompleted(success: false)
                return
            }
            Task { @MainActor [weak self] in
                self?.handleRefresh(refresh)
            }
        }
    }

    private func handleRefresh(_ task: BGAppRefreshTask) {
        scheduleNextRefresh()
        let work = Task {
            await log.info("background refresh fired")
            await engine.requestSync(.backgroundRefresh)
            if !Task.isCancelled {
                task.setTaskCompleted(success: true)
            }
        }
        task.expirationHandler = {
            work.cancel()
            task.setTaskCompleted(success: false)
        }
    }

    func scheduleNextRefresh() {
        let request = BGAppRefreshTaskRequest(identifier: Self.refreshTaskID)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 4 * 3600)
        try? BGTaskScheduler.shared.submit(request) // unsupported on simulator; fine
    }

    // MARK: - Observer queries

    /// (Re)registers observer queries and background delivery for the
    /// currently enabled types. Called at launch and when toggles change.
    func startObservers() async {
        stopObservers()
        for type in TypeRegistry.all {
            guard AppSettings.currentEnabled(type.key) else {
                try? await healthStore.disableBackgroundDelivery(for: type.sampleType)
                continue
            }
            do {
                try await healthStore.enableBackgroundDelivery(for: type.sampleType, frequency: type.frequency)
            } catch {
                await log.warn("background delivery unavailable for \(type.key): \(error.localizedDescription)")
            }
            let key = type.key
            let query = HKObserverQuery(sampleType: type.sampleType, predicate: nil) { [weak self] _, completionHandler, error in
                if error != nil {
                    completionHandler()
                    return
                }
                Task { @MainActor [weak self] in
                    // HealthKit throttles future deliveries if the completion
                    // handler is called before the work is done — so it is
                    // called after the sync pass, from a background task with
                    // a watchdog in case the server hangs.
                    guard let self else {
                        completionHandler()
                        return
                    }
                    await self.observerFired(key)
                    completionHandler()
                }
            }
            healthStore.execute(query)
            observerQueries.append(query)
        }
    }

    func stopObservers() {
        observerQueries.forEach(healthStore.stop)
        observerQueries.removeAll()
    }

    private func observerFired(_ key: String) async {
        let backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "pulso-observer-sync")
        defer {
            if backgroundTask != .invalid {
                UIApplication.shared.endBackgroundTask(backgroundTask)
            }
        }
        if UIApplication.shared.isProtectedDataAvailable {
            await log.info("observer wake: \(key)")
        } else {
            // F2: health data is unreadable while locked. The attempt is
            // still made — it fails cleanly without moving anchors — and the
            // next unlock-time trigger catches up.
            await log.info("observer wake (\(key)) while device locked — will catch up after unlock")
        }
        let engine = engine
        await withTimeout(seconds: 25) {
            await engine.requestSync(.observer(key))
        }
    }
}

/// Runs an operation but returns after `seconds` even if it hasn't finished,
/// cancelling it. Used to guarantee observer completion handlers are called
/// within the background execution window.
func withTimeout(seconds: TimeInterval, _ operation: @escaping @Sendable () async -> Void) async {
    await withTaskGroup(of: Void.self) { group in
        group.addTask { await operation() }
        group.addTask {
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
        }
        await group.next()
        group.cancelAll()
    }
}
