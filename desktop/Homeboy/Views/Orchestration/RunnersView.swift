import Foundation
import SwiftUI

/// One configured runner paired with its admission/operator status, fetched
/// separately since `runner list` and `runner status <id> --full` are
/// distinct CLI calls.
private struct RunnerRowState: Identifiable {
    let runner: HomeboyRunner
    var report: RunnerStatusReport?
    var statusError: String?

    var id: String { runner.id }
}

/// Lists configured runners (`HomeboyCLI.runnerList`) and fans out to each
/// one's admission status (`HomeboyCLI.runnerAdmissionStatus`). Not shared
/// with any other view, so it lives alongside `RunnersView` rather than in a
/// separate ViewModels file.
@MainActor
final class RunnersViewModel: ObservableObject {
    @Published fileprivate private(set) var statuses: [RunnerRowState] = []
    @Published private(set) var isLoading = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared
    private var refreshTask: Task<Void, Never>?

    /// Refreshes immediately, then every 30s while Runners is the visible
    /// tab. Idempotent: calling this while already polling is a no-op.
    func start() {
        guard refreshTask == nil else { return }
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: 30_000_000_000)
            }
        }
    }

    func stop() {
        refreshTask?.cancel()
        refreshTask = nil
    }

    func refresh() async {
        guard !isLoading else { return }
        isLoading = true
        error = nil
        do {
            let runners = try await cli.runnerList()
            var results: [RunnerRowState] = []
            for runner in runners {
                do {
                    let report = try await cli.runnerAdmissionStatus(id: runner.id)
                    results.append(RunnerRowState(runner: runner, report: report, statusError: nil))
                } catch {
                    results.append(RunnerRowState(runner: runner, report: nil, statusError: error.localizedDescription))
                }
            }
            statuses = results
        } catch {
            self.error = error.toDisplayableError(source: "Runners")
        }
        isLoading = false
    }
}

/// Runner connectivity and admission dashboard. Warns when a runner is not
/// accepting jobs, has draining generations, or reports a stale/incompatible
/// daemon; surfaces `next_action` as copyable text either way.
struct RunnersView: View {
    @EnvironmentObject private var navigationState: AppNavigationState
    @StateObject private var viewModel = RunnersViewModel()

    private var isActive: Bool { navigationState.selectedItem == .runners }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
        }
        .frame(minWidth: 720, minHeight: 480)
        .onAppear { if isActive { viewModel.start() } }
        .onDisappear { viewModel.stop() }
        .onChange(of: navigationState.selectedItem) { _, newValue in
            if newValue == .runners {
                viewModel.start()
            } else {
                viewModel.stop()
            }
        }
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Runners")
                    .font(.title2.bold())
                Text("Connectivity and admission state for every configured runner")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if viewModel.isLoading {
                ProgressView()
                    .controlSize(.small)
            }

            Button {
                Task { await viewModel.refresh() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(viewModel.isLoading)
        }
        .padding()
    }

    @ViewBuilder
    private var content: some View {
        if let error = viewModel.error {
            InlineErrorView(error) { viewModel.error = nil }
                .padding([.horizontal, .top])
        }

        if viewModel.isLoading && viewModel.statuses.isEmpty {
            ProgressView("Loading runners...")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if viewModel.statuses.isEmpty {
            ContentUnavailableView(
                "No Runners Configured",
                systemImage: "server.rack",
                description: Text("Configure a runner with `homeboy runner connect`, or press Refresh.")
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(viewModel.statuses) { status in
                        RunnerCard(status: status)
                    }
                }
                .padding()
            }
        }
    }
}

// MARK: - Runner card

private struct RunnerCard: View {
    let status: RunnerRowState

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Image(systemName: "server.rack")
                Text(status.runner.id)
                    .font(.headline)
                Text(status.runner.kind)
                    .font(.caption)
                    .foregroundColor(.secondary)
                Spacer()
                if let summary = status.report?.admissionSummary {
                    connectivityBadge(connected: summary.connected)
                }
            }

            if let statusError = status.statusError {
                Text(statusError)
                    .font(.caption)
                    .foregroundColor(.red)
            } else if let summary = status.report?.admissionSummary {
                admissionRows(summary)
                if summary.hasWarnings {
                    warningBanner
                }
            } else {
                Text("No admission status reported")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            if let nextAction = status.report?.admissionSummary?.nextAction ?? status.report?.operatorSummary?.nextAction {
                nextActionRow(nextAction)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(10)
    }

    private func connectivityBadge(connected: Bool) -> some View {
        Label(connected ? "Connected" : "Disconnected", systemImage: connected ? "checkmark.circle.fill" : "xmark.circle.fill")
            .font(.caption.bold())
            .foregroundColor(connected ? .green : .red)
    }

    private func admissionRows(_ summary: RunnerAdmissionSummary) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 16) {
                statField("Accepting Jobs", summary.acceptingJobs ? "Yes" : "No", warn: !summary.acceptingJobs)
                statField("Active Jobs", "\(summary.activeJobCount)")
                statField("Draining", "\(summary.drainingGenerationCount)", warn: summary.drainingGenerationCount > 0)
            }
            HStack(spacing: 16) {
                statField("Daemon Fresh", summary.daemonFresh ? "Yes" : "No", warn: !summary.daemonFresh)
                statField("Daemon Compatible", summary.daemonCompatible ? "Yes" : "No", warn: !summary.daemonCompatible)
            }
            if let identity = summary.daemonBuildIdentity {
                Text(identity)
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
                    .textSelection(.enabled)
            }
        }
    }

    private func statField(_ label: String, _ value: String, warn: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label)
                .font(.caption2)
                .foregroundColor(.secondary)
            Text(value)
                .font(.caption.bold())
                .foregroundColor(warn ? .orange : .primary)
        }
    }

    private var warningBanner: some View {
        HStack(spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            Text("Runner needs attention")
                .font(.caption)
                .foregroundColor(.orange)
        }
        .padding(6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.12))
        .cornerRadius(6)
    }

    private func nextActionRow(_ action: String) -> some View {
        HStack {
            Text(action)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .lineLimit(2)
            Spacer()
            CopyButton(content: ConsoleOutput(action, source: "Runner Next Action: \(status.runner.id)"), style: .icon)
        }
        .padding(6)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }
}

#Preview {
    RunnersView()
        .environmentObject(AppNavigationState())
}
