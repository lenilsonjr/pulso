import SwiftUI

struct LogView: View {
    let services: AppServices

    var body: some View {
        List(services.status.recentLog) { entry in
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.message)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(color(for: entry.level))
                Text(entry.date, format: .dateTime.day().month().hour().minute().second())
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .navigationTitle("Activity Log")
        .overlay {
            if services.status.recentLog.isEmpty {
                ContentUnavailableView(
                    "No activity yet",
                    systemImage: "text.alignleft",
                    description: Text("Sync activity appears here, including background runs.")
                )
            }
        }
    }

    private func color(for level: LogEntry.Level) -> Color {
        switch level {
        case .info: .primary
        case .warn: .orange
        case .error: .red
        }
    }
}
