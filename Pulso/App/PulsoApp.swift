import SwiftUI

@main
struct PulsoApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView(services: delegate.services)
        }
        .onChange(of: scenePhase) { _, phase in
            delegate.services.scenePhase(phase)
        }
    }
}

@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate {
    let services = AppServices()

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        services.launch()
        return true
    }
}
