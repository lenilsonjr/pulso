import Foundation
import HealthKit
import SwiftUI

/// Composition root: owns the four components and the lifecycle glue.
@MainActor
final class AppServices {
    let healthStore = HKHealthStore()
    let settings = AppSettings()
    let status = AppStatus()
    let log: LogStore
    let anchors: AnchorStore
    let outbox: Outbox
    let engine: SyncEngine
    let hub: TriggerHub
    private let uploader = HTTPUploader()

    init() {
        let fm = FileManager.default
        let support = (try? fm.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        )) ?? fm.temporaryDirectory
        var root = support.appendingPathComponent("Pulso", isDirectory: true)
        try? fm.createDirectory(at: root, withIntermediateDirectories: true)
        // Pending health data must not leak into device backups.
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? root.setResourceValues(values)

        log = LogStore(directory: root)
        anchors = AnchorStore(directory: root.appendingPathComponent("Anchors", isDirectory: true))
        outbox = Outbox(
            directory: root.appendingPathComponent("Outbox", isDirectory: true),
            anchors: anchors,
            uploader: uploader,
            target: { AppSettings.currentTarget() },
            log: log,
            status: status
        )
        engine = SyncEngine(healthStore: healthStore, anchors: anchors, outbox: outbox, log: log, status: status)
        hub = TriggerHub(healthStore: healthStore, engine: engine, log: log)

        status.load()
        let status = status
        let log = log
        Task {
            let entries = await log.tail(200)
            status.seedLog(entries)
            await log.setSink { entry in
                Task { @MainActor in status.push(entry) }
            }
        }
    }

    private var isTestRun: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    /// Called from application(_:didFinishLaunchingWithOptions:). Also runs
    /// when HealthKit relaunches the app in the background for delivery.
    func launch() {
        guard HKHealthStore.isHealthDataAvailable() else {
            status.healthUnavailable = true
            return
        }
        guard !isTestRun else { return }
        hub.registerBackgroundRefresh()
        hub.scheduleNextRefresh()
        Task {
            await hub.startObservers()
            await refreshAuthStatus()
            await refreshDerived()
        }
    }

    func scenePhase(_ phase: ScenePhase) {
        guard !isTestRun, !status.healthUnavailable else { return }
        switch phase {
        case .active:
            Task {
                await refreshAuthStatus()
                await syncNow(.foreground)
            }
        case .background:
            hub.scheduleNextRefresh()
        default:
            break
        }
    }

    func syncNow(_ reason: SyncReason = .manual) async {
        // Backfill can take a while; don't let the screen lock kill it (F2).
        UIApplication.shared.isIdleTimerDisabled = true
        defer { UIApplication.shared.isIdleTimerDisabled = false }
        await engine.requestSync(reason)
        await refreshDerived()
    }

    func requestHealthAccess() async {
        do {
            try await healthStore.requestAuthorization(toShare: [], read: TypeRegistry.readTypes)
        } catch {
            await log.error("authorization request failed: \(error.localizedDescription)")
        }
        await refreshAuthStatus()
        await syncNow(.manual)
    }

    func refreshAuthStatus() async {
        guard !status.healthUnavailable else { return }
        do {
            let request = try await healthStore.statusForAuthorizationRequest(toShare: [], read: TypeRegistry.readTypes)
            status.authNeeded = request == .shouldRequest
        } catch {
            // Typically a signing problem (missing HealthKit entitlement) —
            // surface it, or "no permission" becomes indistinguishable from a
            // build issue.
            status.authNeeded = false
            await log.warn("could not determine Health authorization status: \(error.localizedDescription)")
        }
    }

    func refreshDerived() async {
        status.needsInitialImport = AppSettings.currentEnabledTypes().contains { !anchors.hasAnchor($0.key) }
        await outbox.refreshPendingStatus()
    }

    func typeTogglesChanged() {
        Task {
            await hub.startObservers()
            await engine.requestSync(.settingsChanged)
            await refreshDerived()
        }
    }

    /// Clears all sync state and re-sends everything. Safe (the server
    /// dedupes by uuid) but slow — guarded by a confirmation in Settings.
    func reimportAll() async {
        await log.warn("full re-import requested — clearing anchors and outbox")
        await outbox.removeAll()
        anchors.clearAll()
        await engine.resetAllAnchors()
        status.resetCounters()
        await syncNow(.manual)
    }

    func testConnection() async -> String {
        guard let url = settings.healthURL else {
            return "Enter a valid server URL first"
        }
        return await uploader.checkHealth(url)
    }
}
