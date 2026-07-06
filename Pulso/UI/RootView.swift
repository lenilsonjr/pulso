import SwiftUI

struct RootView: View {
    let services: AppServices

    var body: some View {
        if services.status.healthUnavailable {
            ContentUnavailableView(
                "Health Data Unavailable",
                systemImage: "heart.slash",
                description: Text("This device does not provide HealthKit data.")
            )
        } else {
            TabView {
                StatusView(services: services)
                    .tabItem { Label("Status", systemImage: "waveform.path.ecg") }
                SettingsView(services: services)
                    .tabItem { Label("Settings", systemImage: "gearshape") }
            }
        }
    }
}
