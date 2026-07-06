import SwiftUI

struct SettingsView: View {
    let services: AppServices

    @State private var testResult: String?
    @State private var testing = false
    @State private var confirmReimport = false

    var body: some View {
        @Bindable var settings = services.settings
        NavigationStack {
            Form {
                Section {
                    TextField("http://100.x.y.z:8787", text: $settings.endpoint)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("Bearer token (optional)", text: $settings.token)
                    Button {
                        testing = true
                        testResult = nil
                        Task {
                            testResult = await services.testConnection()
                            testing = false
                        }
                    } label: {
                        HStack {
                            Text("Test Connection")
                            if testing { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(testing)
                    if let testResult {
                        Text(testResult)
                            .font(.footnote)
                            .foregroundStyle(testResult.hasPrefix("OK") ? .green : .red)
                    }
                } header: {
                    Text("Server")
                } footer: {
                    Text("Base URL of your Pulso server — every sample goes there and nowhere else. Plain HTTP is fine on a Tailscale network; the tunnel is already encrypted.")
                }

                Section {
                    ForEach(TypeRegistry.all) { type in
                        Toggle(type.displayName, isOn: enabledBinding(type.key))
                    }
                } header: {
                    Text("Data Types")
                } footer: {
                    Text("Disabling a type stops new deliveries; data already on your server stays there.")
                }

                Section {
                    Button("Re-import Full History", role: .destructive) {
                        confirmReimport = true
                    }
                    .confirmationDialog("Re-import everything?", isPresented: $confirmReimport) {
                        Button("Re-import", role: .destructive) {
                            Task { await services.reimportAll() }
                        }
                    } message: {
                        Text("Clears local sync state and sends your entire history again. Your server deduplicates by sample ID, so this is safe — just slow.")
                    }
                } header: {
                    Text("Maintenance")
                }

                Section {
                    LabeledContent("Version", value: appVersion)
                } header: {
                    Text("About")
                } footer: {
                    Text("Pulso reads the Apple Health data you select and delivers it to the server you configure. No cloud, no accounts, no analytics.")
                }
            }
            .navigationTitle("Settings")
        }
    }

    private func enabledBinding(_ key: String) -> Binding<Bool> {
        Binding {
            services.settings.isEnabled(key)
        } set: { value in
            services.settings.setEnabled(key, value)
            services.typeTogglesChanged()
        }
    }

    private var appVersion: String {
        let short = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
        return "\(short) (\(build))"
    }
}
