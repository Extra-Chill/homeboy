import Foundation
import SwiftUI

/// Landing view for the orchestration shell: recent runs across every
/// mission, newest first, each tagged with a state badge. Selecting a run
/// switches navigation to Missions with that run pre-selected, via
/// `AppNavigationState`.
struct ActivityView: View {
    @EnvironmentObject private var missionStore: MissionStore
    @EnvironmentObject private var navigationState: AppNavigationState

    @State private var refreshTask: Task<Void, Never>?

    private var isActive: Bool { navigationState.selectedItem == .activity }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
        }
        .frame(minWidth: 640, minHeight: 480)
        .onAppear { if isActive { start() } }
        .onDisappear { stop() }
        .onChange(of: navigationState.selectedItem) { _, newValue in
            if newValue == .activity {
                start()
            } else {
                stop()
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Activity")
                    .font(.title2.bold())
                Text("Recent runs across every mission, newest first")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if missionStore.isLoadingRuns {
                ProgressView()
                    .controlSize(.small)
            }

            Button {
                Task { await missionStore.loadRuns(reset: true) }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(missionStore.isLoadingRuns)
        }
        .padding()
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if let error = missionStore.error {
            InlineErrorView(error) { missionStore.error = nil }
                .padding([.horizontal, .top])
        }

        if missionStore.isLoadingRuns && missionStore.runsById.isEmpty {
            ProgressView("Loading activity...")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if missionStore.allRunsNewestFirst.isEmpty {
            ContentUnavailableView(
                "No Activity Yet",
                systemImage: "waveform.path.ecg",
                description: Text("Runs submitted to Homeboy core will show up here.")
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            List(missionStore.allRunsNewestFirst) { run in
                ActivityRunRow(run: run)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        navigationState.openRun(run.run)
                    }
            }
            .listStyle(.inset)
        }
    }

    // MARK: - Polling

    /// Refreshes immediately, then every 15s while Activity is the visible
    /// tab. `MissionStore.loadRuns` is idempotent-guarded (`isLoadingRuns`),
    /// so an overlapping tick is simply skipped.
    private func start() {
        guard refreshTask == nil else { return }
        refreshTask = Task {
            while !Task.isCancelled {
                await missionStore.loadRuns(reset: true)
                try? await Task.sleep(nanoseconds: 15_000_000_000)
            }
        }
    }

    private func stop() {
        refreshTask?.cancel()
        refreshTask = nil
    }
}

// MARK: - Row

private struct ActivityRunRow: View {
    let run: ControlPlaneRun

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            RunStateBadge(state: run.state)

            VStack(alignment: .leading, spacing: 2) {
                Text(run.mission ?? run.run)
                    .font(.body.monospaced())
                    .lineLimit(1)
                if run.mission != nil {
                    Text(run.run)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                }
                if let blocker = run.blocker {
                    Text(blocker.message)
                        .font(.caption)
                        .foregroundColor(.orange)
                        .lineLimit(2)
                }
            }

            Spacer()

            VStack(alignment: .trailing, spacing: 2) {
                if let created = ControlPlaneDate.relative(run.createdAt) {
                    Text(created)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                if let phase = run.phase {
                    Text(phase)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
        }
        .padding(.vertical, 4)
    }
}

// MARK: - Shared state badge

/// Small colored pill for a run's open-ended `state` string. Shared by
/// Activity and Missions. Unrecognized states fall back to a neutral color
/// rather than failing to render, matching the open-ended decoding contract
/// documented on `ControlPlaneRun`.
struct RunStateBadge: View {
    let state: String

    var body: some View {
        Text(state.replacingOccurrences(of: "_", with: " "))
            .font(.caption2.bold())
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(color.opacity(0.16))
            .foregroundColor(color)
            .cornerRadius(6)
    }

    private var color: Color {
        switch state {
        case "succeeded", "candidate_recoverable", "resolved", "promoted", "reconciled":
            return .green
        case "failed", "error", "cancelled", "quarantined":
            return .red
        case "running", "in_progress", "executing":
            return .blue
        case "queued", "pending", "blocked", "verification_pending":
            return .orange
        default:
            return .secondary
        }
    }
}

#Preview {
    ActivityView()
        .environmentObject(MissionStore())
        .environmentObject(AppNavigationState())
}
