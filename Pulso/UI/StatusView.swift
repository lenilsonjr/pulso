import SwiftUI

struct StatusView: View {
    let services: AppServices

    private var status: AppStatus { services.status }

    var body: some View {
        NavigationStack {
            List {
                if status.authNeeded {
                    Section {
                        Button {
                            Task { await services.requestHealthAccess() }
                        } label: {
                            Label("Grant Health Access", systemImage: "heart.text.square")
                                .font(.headline)
                        }
                    } footer: {
                        Text("Pulso only asks to read the types listed below. iOS never reveals what you granted — only your data (or its absence) does.")
                    }
                }

                if services.settings.baseURL == nil {
                    Section {
                        Label("Set your server address in Settings", systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                    }
                }

                Section {
                    Button {
                        Task { await services.syncNow(.manual) }
                    } label: {
                        HStack {
                            if status.syncing {
                                ProgressView()
                                    .padding(.trailing, 4)
                            } else {
                                Image(systemName: "arrow.triangle.2.circlepath")
                            }
                            Text(status.syncing ? "Syncing…" : "Sync Now")
                                .font(.headline)
                        }
                    }
                    .disabled(status.syncing)
                } footer: {
                    if status.needsInitialImport {
                        Text("First sync imports your full history. Keep the app open and unlocked until it finishes — progress is saved continuously, so interrupting is safe.")
                    }
                }

                Section("Data Types") {
                    ForEach(enabledTypes) { type in
                        typeRow(type)
                    }
                }

                Section("Delivery") {
                    LabeledContent("Pending batches", value: "\(status.outboxPending)")
                    if let retry = status.outboxNextRetry, retry > Date() {
                        LabeledContent("Next retry") {
                            Text(retry, style: .relative)
                        }
                    }
                    if let response = status.lastResponse {
                        LabeledContent("Last server response") {
                            Text(response)
                                .font(.caption)
                                .lineLimit(2)
                                .multilineTextAlignment(.trailing)
                        }
                    }
                }

                Section {
                    NavigationLink("Activity Log") {
                        LogView(services: services)
                    }
                }
            }
            .navigationTitle("Pulso")
            .refreshable {
                await services.syncNow(.manual)
            }
        }
    }

    private var enabledTypes: [SyncedType] {
        TypeRegistry.all.filter { services.settings.isEnabled($0.key) }
    }

    @ViewBuilder
    private func typeRow(_ type: SyncedType) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(type.displayName)
            Group {
                if let ts = status.typeStatus[type.key], ts.totalDelivered > 0 {
                    HStack(spacing: 4) {
                        Text("\(ts.totalDelivered) delivered")
                        if let checked = ts.lastChecked {
                            Text("· checked")
                            Text(checked, style: .relative)
                            Text("ago")
                        }
                    }
                } else if status.typeStatus[type.key]?.lastChecked != nil {
                    // F1: HealthKit won't say whether read access was denied —
                    // "no data" and "no permission" look identical.
                    Text("No data yet — or Health access not granted (Health app → Sharing)")
                } else {
                    Text("Waiting for first sync")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }
}
