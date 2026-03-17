import SwiftUI

/// Displays config gaps with actionable fix buttons
/// Shown in component detail or as a dedicated panel
struct ConfigGapsView: View {
    let gaps: [ConfigGapDetail]
    let onFix: ((ConfigGapDetail) async -> Void)?

    @State private var fixingGapId: String?
    @State private var fixedGapIds: Set<String> = []
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Header
            HStack {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.orange)
                Text("Configuration Issues")
                    .font(.headline)
                Spacer()
                Text("\(gaps.count) gap\(gaps.count == 1 ? "" : "s")")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            // Gap list
            if gaps.isEmpty {
                ContentUnavailableView(
                    "No Configuration Issues",
                    systemImage: "checkmark.circle.fill",
                    description: Text("All components are properly configured")
                )
                .foregroundColor(.green)
            } else {
                VStack(spacing: 8) {
                    ForEach(gaps) { gap in
                        ConfigGapRow(
                            gap: gap,
                            isFixing: fixingGapId == gap.id,
                            isFixed: fixedGapIds.contains(gap.id),
                            onFix: { await fixGap(gap) }
                        )
                    }
                }
            }

            // Error message
            if let error = errorMessage {
                HStack {
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundColor(.red)
                    Text(error)
                        .font(.caption)
                        .foregroundColor(.red)
                    Spacer()
                    Button("Dismiss") {
                        errorMessage = nil
                    }
                    .buttonStyle(.borderless)
                }
                .padding(.vertical, 4)
            }
        }
        .padding()
        .background(Color(.systemBackground))
        .cornerRadius(8)
        .shadow(radius: 1)
    }

    private func fixGap(_ gap: ConfigGapDetail) async {
        guard let onFix = onFix else { return }

        fixingGapId = gap.id
        errorMessage = nil

        do {
            await onFix(gap)
            fixedGapIds.insert(gap.id)
        } catch {
            errorMessage = "Failed to fix: \(error.localizedDescription)"
        }

        fixingGapId = nil
    }
}

/// Individual config gap row with fix button
struct ConfigGapRow: View {
    let gap: ConfigGapDetail
    let isFixing: Bool
    let isFixed: Bool
    let onFix: () async -> Void

    var body: some View {
        HStack(spacing: 12) {
            // Icon based on field type
            Image(systemName: iconForField(gap.field))
                .foregroundColor(colorForField(gap.field))
                .frame(width: 24)

            VStack(alignment: .leading, spacing: 4) {
                // Component and field
                HStack {
                    Text(gap.componentId)
                        .font(.subheadline)
                        .fontWeight(.semibold)
                    Text("·")
                        .foregroundColor(.secondary)
                    Text(gap.field)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color(.systemGray6))
                        .cornerRadius(4)
                }

                // Reason
                Text(gap.reason)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(2)

                // Command preview
                if !isFixed {
                    Text(gap.command)
                        .font(.caption2)
                        .fontWeight(.medium)
                        .foregroundColor(.blue)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.blue.opacity(0.1))
                        .cornerRadius(4)
                }
            }

            Spacer()

            // Fix button or status
            if isFixed {
                Label("Fixed", systemImage: "checkmark.circle.fill")
                    .font(.caption)
                    .foregroundColor(.green)
            } else if isFixing {
                ProgressView()
                    .controlSize(.small)
            } else {
                Button {
                    Task {
                        await onFix()
                    }
                } label: {
                    Label("Fix", systemImage: "wrench.and.screwdriver")
                        .font(.caption)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
        }
        .padding()
        .background(Color(.secondarySystemBackground))
        .cornerRadius(8)
        .opacity(isFixed ? 0.7 : 1.0)
    }

    private func iconForField(_ field: String) -> String {
        switch field {
        case "extensions":
            return "puzzlepiece.extension"
        case "buildCommand":
            return "hammer"
        case "versionTargets":
            return "tag"
        case "changelogTarget":
            return "doc.text"
        default:
            return "gear.badge.questionmark"
        }
    }

    private func colorForField(_ field: String) -> Color {
        switch field {
        case "extensions":
            return .blue
        case "buildCommand":
            return .orange
        case "versionTargets":
            return .green
        case "changelogTarget":
            return .purple
        default:
            return .gray
        }
    }
}

/// Badge showing gap count for component list
struct ConfigGapBadge: View {
    let count: Int

    var body: some View {
        if count > 0 {
            HStack(spacing: 4) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.caption2)
                Text("\(count)")
                    .font(.caption2)
                    .fontWeight(.semibold)
            }
            .foregroundColor(.white)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Color.orange)
            .cornerRadius(4)
        }
    }
}

// MARK: - Preview

#Preview("Config Gaps View") {
    let sampleGaps = [
        ConfigGapDetail(
            componentId: "extra-chill-blog",
            field: "extensions",
            reason: "No extension configured. Extension commands (lint, test, build) require an extension.",
            command: "homeboy component set extra-chill-blog --extension nodejs"
        ),
        ConfigGapDetail(
            componentId: "extra-chill-theme",
            field: "buildCommand",
            reason: "build.sh exists but no build command configured",
            command: "homeboy component set extra-chill-theme --build-command \"./build.sh\""
        )
    ]

    ConfigGapsView(
        gaps: sampleGaps,
        onFix: { gap in
            print("Fixing: \(gap.command)")
            try? await Task.sleep(nanoseconds: 500_000_000)
        }
    )
    .padding()
    .frame(width: 600, height: 400)
}

#Preview("Config Gap Badge") {
    HStack {
        ConfigGapBadge(count: 3)
        ConfigGapBadge(count: 1)
        ConfigGapBadge(count: 0)
    }
    .padding()
}
