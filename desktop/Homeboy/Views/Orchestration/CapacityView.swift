import Foundation
import SwiftUI

/// Polls `homeboy agent-task capacity` (via `HomeboyCLI`, since capacity has
/// no daemon HTTP route yet) and holds the most recent report for
/// `CapacityView`. Not shared with any other view, so it lives alongside its
/// view rather than in a separate ViewModels file.
@MainActor
final class CapacityViewModel: ObservableObject {
    @Published private(set) var report: AgentTaskCapacityReport?
    @Published private(set) var isLoading = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared
    private var refreshTask: Task<Void, Never>?

    /// Refreshes immediately, then every 30s while Capacity is the visible
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
            report = try await cli.agentTaskCapacity()
        } catch {
            self.error = error.toDisplayableError(source: "Capacity")
        }
        isLoading = false
    }
}

/// One card per capacity route, titled by scope suffix / models / backend
/// (in that priority order), showing the headline and a row per account.
struct CapacityView: View {
    @EnvironmentObject private var navigationState: AppNavigationState
    @StateObject private var viewModel = CapacityViewModel()

    private var isActive: Bool { navigationState.selectedItem == .capacity }

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
            if newValue == .capacity {
                viewModel.start()
            } else {
                viewModel.stop()
            }
        }
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Capacity")
                    .font(.title2.bold())
                Text(nextResetLine)
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

    private var nextResetLine: String {
        guard let nextReset = viewModel.report?.nextReset else {
            return "Provider and account capacity reported by `homeboy agent-task capacity`"
        }
        let relative = ControlPlaneDate.relative(nextReset) ?? "soon"
        let local = ControlPlaneDate.localTimeString(nextReset) ?? nextReset
        return "Next plan frees up \(relative) (\(local))"
    }

    @ViewBuilder
    private var content: some View {
        if let error = viewModel.error {
            InlineErrorView(error) { viewModel.error = nil }
                .padding([.horizontal, .top])
        }

        if viewModel.isLoading && viewModel.report == nil {
            ProgressView("Loading capacity...")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let report = viewModel.report, !report.routes.isEmpty {
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 280), spacing: 16)], alignment: .leading, spacing: 16) {
                    ForEach(report.routes) { route in
                        CapacityRouteCard(route: route)
                    }
                }
                .padding()
            }
        } else {
            ContentUnavailableView(
                "No Capacity Data",
                systemImage: "gauge.with.dots.needle.50percent",
                description: Text("Run `homeboy agent-task capacity` from the CLI, or press Refresh.")
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

// MARK: - Route card

private struct CapacityRouteCard: View {
    let route: AgentTaskCapacityRoute

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.headline)
            Text(route.capacity.headline)
                .font(.subheadline)
                .foregroundColor(headlineColor)

            Divider()

            if let accounts = route.capacity.accounts, !accounts.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(accounts) { account in
                        accountRow(account)
                    }
                }
            } else {
                Text("No account detail published")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(10)
    }

    private func accountRow(_ account: AgentTaskCapacityAccount) -> some View {
        HStack {
            Text(account.account)
                .font(.caption.monospaced())
                .lineLimit(1)
            Text(account.state)
                .font(.caption2)
                .foregroundColor(stateColor(account.state))
            Spacer()
            if let remaining = account.remaining {
                Text("\(remaining)%")
                    .font(.caption.monospacedDigit())
            }
            if let relative = account.relativeReset {
                Text(relative)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
        }
    }

    /// Scope suffix (`opencode:anthropic` -> `anthropic`), else models, else
    /// backend, matching the reuse instruction: `AgentTaskCapacityRoute
    /// .scopeSuffix` already implements the prefix-stripping.
    private var title: String {
        if !route.scope.isEmpty {
            return route.scopeSuffix
        }
        if !route.models.isEmpty {
            return route.models.joined(separator: ", ")
        }
        return route.backend
    }

    private var headlineColor: Color {
        switch route.capacity.state {
        case "known": return .primary
        case "exhausted": return .red
        default: return .secondary
        }
    }

    private func stateColor(_ state: String) -> Color {
        switch state {
        case "available": return .green
        case "exhausted": return .red
        case "credential_expired": return .orange
        default: return .secondary
        }
    }
}

#Preview {
    CapacityView()
        .environmentObject(AppNavigationState())
}
