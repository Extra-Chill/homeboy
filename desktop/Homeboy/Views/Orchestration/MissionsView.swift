import Foundation
import SwiftUI

/// Mission/run explorer: a sidebar of runs grouped by mission (from
/// `MissionStore`), and a detail pane with the selected run's header,
/// blocker, candidate state, event timeline, artifacts, action buttons
/// generated from `action_eligibility`, and a raw JSON inspector built from
/// the review evidence blob.
struct MissionsView: View {
    @EnvironmentObject private var missionStore: MissionStore
    @EnvironmentObject private var navigationState: AppNavigationState

    @State private var pendingAction: ControlPlaneRunAction?
    @State private var actionInputs: [String: String] = [:]
    @State private var isPerformingAction = false
    @State private var actionError: (any DisplayableError)?
    @State private var showRawJSON = false

    private var isActive: Bool { navigationState.selectedItem == .missions }

    private var selectionBinding: Binding<String?> {
        Binding(
            get: { missionStore.selectedRunId },
            set: { missionStore.selectRun($0) }
        )
    }

    var body: some View {
        NavigationSplitView {
            sidebar
        } detail: {
            detail
        }
        .frame(minWidth: 960, minHeight: 640)
        .onAppear { if isActive { activate() } }
        .onDisappear { deactivate() }
        .onChange(of: navigationState.selectedItem) { _, newValue in
            if newValue == .missions {
                activate()
            } else {
                deactivate()
            }
        }
    }

    // MARK: - Sidebar

    private var sidebar: some View {
        List(selection: selectionBinding) {
            if missionsWithRuns.isEmpty {
                if missionStore.isLoadingMissions || missionStore.isLoadingRuns {
                    ProgressView("Loading missions...")
                } else {
                    Text("No missions yet")
                        .foregroundColor(.secondary)
                }
            }

            ForEach(missionsWithRuns, id: \.self) { missionId in
                Section(missionId) {
                    ForEach(missionStore.runs(forMission: missionId)) { run in
                        HStack {
                            RunStateBadge(state: run.state)
                            Text(run.run)
                                .font(.caption.monospaced())
                                .lineLimit(1)
                        }
                        .tag(run.run)
                    }
                }
            }
        }
        .navigationTitle("Missions")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task {
                        await missionStore.loadMissions()
                        await missionStore.loadRuns()
                    }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(missionStore.isLoadingMissions || missionStore.isLoadingRuns)
            }
        }
        .frame(minWidth: 260)
    }

    /// Missions with at least one loaded run, newest run first.
    private var missionsWithRuns: [String] {
        missionStore.runsByMission.keys.sorted { lhs, rhs in
            let lhsLatest = missionStore.runs(forMission: lhs).first.flatMap { ControlPlaneDate.parse($0.createdAt) } ?? .distantPast
            let rhsLatest = missionStore.runs(forMission: rhs).first.flatMap { ControlPlaneDate.parse($0.createdAt) } ?? .distantPast
            return lhsLatest > rhsLatest
        }
    }

    // MARK: - Detail

    @ViewBuilder
    private var detail: some View {
        if let run = missionStore.selectedRun {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let error = missionStore.error {
                        InlineErrorView(error) { missionStore.error = nil }
                    }
                    if let actionError {
                        InlineErrorView(actionError) { self.actionError = nil }
                    }

                    header(run)
                    if let blocker = run.blocker {
                        blockerCallout(blocker)
                    }
                    candidateSection(run)
                    actionsSection(run)
                    eventsSection
                    artifactsSection(run)
                    rawJSONSection
                }
                .padding()
            }
            .confirmationDialog(
                pendingAction?.displayName ?? "",
                isPresented: simpleConfirmationBinding,
                titleVisibility: .visible
            ) {
                Button(pendingAction?.displayName ?? "Confirm", role: .destructive) {
                    if let action = pendingAction {
                        Task { await performAction(action, inputs: [:]) }
                    }
                    pendingAction = nil
                }
                Button("Cancel", role: .cancel) { pendingAction = nil }
            } message: {
                Text(pendingAction?.reason ?? "")
            }
            .sheet(item: inputSheetBinding) { action in
                actionInputSheet(action)
            }
        } else if missionStore.isLoadingRuns || missionStore.isLoadingMissions {
            ProgressView("Loading missions...")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView(
                "Select a Run",
                systemImage: "point.3.connected.trianglepath.dotted",
                description: Text("Choose a run from the sidebar to inspect its state, events, and artifacts.")
            )
        }
    }

    private func header(_ run: ControlPlaneRun) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                RunStateBadge(state: run.state)
                Text(run.run)
                    .font(.title3.bold())
                    .textSelection(.enabled)
                Spacer()
                if missionStore.isLoadingEvents {
                    ProgressView()
                        .controlSize(.small)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                summaryRow("Phase", run.phase ?? "-")
                summaryRow("Provider", run.provider?.id ?? "-")
                summaryRow("Placement", placementText(run.placement))
                summaryRow("Created", timestamp(run.createdAt))
                summaryRow("Updated", timestamp(run.updatedAt))
                summaryRow("Finished", timestamp(run.finishedAt))
                summaryRow("Heartbeat", timestamp(run.heartbeatAt))
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private func summaryRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.caption.bold())
                .foregroundColor(.secondary)
                .frame(width: 90, alignment: .leading)
            Text(value)
                .textSelection(.enabled)
            Spacer()
        }
    }

    private func placementText(_ placement: ControlPlanePlacement?) -> String {
        guard let placement else { return "-" }
        var parts: [String] = []
        if let selected = placement.selected { parts.append("selected: \(selected)") }
        if let requested = placement.requested, requested != placement.selected {
            parts.append("requested: \(requested)")
        }
        if let runnerId = placement.runnerId { parts.append("runner: \(runnerId)") }
        return parts.isEmpty ? "-" : parts.joined(separator: ", ")
    }

    private func timestamp(_ value: String?) -> String {
        ControlPlaneDate.localTimeString(value) ?? "-"
    }

    private func blockerCallout(_ blocker: ControlPlaneBlocker) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            VStack(alignment: .leading, spacing: 4) {
                Text(blocker.code ?? "Blocked")
                    .font(.caption.bold())
                Text(blocker.message)
                    .textSelection(.enabled)
                if let reason = blocker.reason {
                    Text(reason)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            Spacer()
        }
        .padding()
        .background(Color.orange.opacity(0.12))
        .cornerRadius(8)
    }

    private func candidateSection(_ run: ControlPlaneRun) -> some View {
        GroupBox("Candidate") {
            if let candidate = run.candidate {
                HStack {
                    Text(candidate.state)
                        .font(.body.monospaced())
                    if let id = candidate.id {
                        Text(id)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .textSelection(.enabled)
                    }
                    Spacer()
                }
            } else {
                Text("No candidate reported")
                    .foregroundColor(.secondary)
            }
        }
    }

    // MARK: - Actions

    private func actionsSection(_ run: ControlPlaneRun) -> some View {
        GroupBox("Actions") {
            let actions = run.actionEligibility?.actions ?? []
            if actions.isEmpty {
                Text("No actions reported for this run")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(actions) { action in
                        actionButton(action)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    @ViewBuilder
    private func actionButton(_ action: ControlPlaneRunAction) -> some View {
        let button = Button {
            tapped(action)
        } label: {
            HStack {
                Text(action.displayName)
                if action.requiresConfirmation {
                    Image(systemName: "exclamationmark.circle")
                        .font(.caption)
                }
            }
        }
        .buttonStyle(.bordered)
        .disabled(!action.isAvailable || isPerformingAction)

        if action.isAvailable {
            button
        } else {
            button.help(action.reason)
        }
    }

    private var simpleConfirmationBinding: Binding<Bool> {
        Binding(
            get: { pendingAction != nil && (pendingAction?.requiredInputs?.isEmpty ?? true) },
            set: { if !$0 { pendingAction = nil } }
        )
    }

    private var inputSheetBinding: Binding<ControlPlaneRunAction?> {
        Binding(
            get: { (pendingAction?.requiredInputs?.isEmpty == false) ? pendingAction : nil },
            set: { if $0 == nil { pendingAction = nil } }
        )
    }

    private func actionInputSheet(_ action: ControlPlaneRunAction) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(action.displayName)
                .font(.headline)
            Text(action.reason)
                .font(.caption)
                .foregroundColor(.secondary)

            ForEach(action.requiredInputs ?? [], id: \.self) { input in
                TextField(input, text: Binding(
                    get: { actionInputs[input] ?? "" },
                    set: { actionInputs[input] = $0 }
                ))
                .textFieldStyle(.roundedBorder)
            }

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { pendingAction = nil }
                Button("Confirm") {
                    Task { await performAction(action, inputs: actionInputs) }
                    pendingAction = nil
                }
                .buttonStyle(.borderedProminent)
                .disabled((action.requiredInputs ?? []).contains { (actionInputs[$0] ?? "").isEmpty })
            }
        }
        .padding()
        .frame(minWidth: 320)
    }

    private func tapped(_ action: ControlPlaneRunAction) {
        guard action.isAvailable, !isPerformingAction else { return }
        if action.requiresConfirmation || !(action.requiredInputs?.isEmpty ?? true) {
            actionInputs = Dictionary(uniqueKeysWithValues: (action.requiredInputs ?? []).map { ($0, "") })
            pendingAction = action
        } else {
            Task { await performAction(action, inputs: [:]) }
        }
    }

    private func performAction(_ action: ControlPlaneRunAction, inputs: [String: String]) async {
        guard let runId = missionStore.selectedRunId else { return }
        isPerformingAction = true
        defer { isPerformingAction = false }

        do {
            let updated = try await ControlPlaneClient.shared.performAction(
                runId: runId,
                action: action.action,
                parameters: parameters(for: action, inputs: inputs),
                idempotencyKey: UUID().uuidString
            )
            missionStore.applyUpdatedRun(updated)
            await missionStore.refreshSelectedRun()
        } catch {
            actionError = error.toDisplayableError(source: "Missions")
        }
        actionInputs = [:]
    }

    /// Maps a reported action name to the parameter payload core expects.
    /// Actions core has already modeled in `ControlPlaneActionParameters`
    /// use their typed case; anything else falls back to a raw payload built
    /// from `required_inputs`, using the same `homeboy/control-plane-<action>
    /// -parameters/v1` schema naming core uses for the modeled cases.
    private func parameters(for action: ControlPlaneRunAction, inputs: [String: String]) -> ControlPlaneActionParameters {
        switch action.action {
        case "cancel":
            return .cancel(reason: inputs["reason"])
        case "retry":
            return .retry(force: inputs["force"] == "true")
        case "quarantine":
            return .quarantine(reason: inputs["reason"] ?? "")
        case "placement_update":
            return .placementUpdate(placement: inputs["placement"] ?? "")
        default:
            guard !inputs.isEmpty else { return .empty }
            let schema = "homeboy/control-plane-\(action.action.replacingOccurrences(of: "_", with: "-"))-parameters/v1"
            return .raw(schema: schema, data: inputs)
        }
    }

    // MARK: - Events

    private var eventsSection: some View {
        GroupBox("Event Timeline") {
            if missionStore.selectedRunEvents.isEmpty {
                Text(missionStore.isLoadingEvents ? "Loading events..." : "No events yet")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(missionStore.selectedRunEvents) { event in
                        eventRow(event)
                    }
                }
            }
        }
    }

    private func eventRow(_ event: ControlPlaneEvent) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text("#\(event.sequence)")
                    .font(.caption.monospaced())
                    .foregroundColor(.secondary)
                Text(event.kind)
                    .font(.caption.bold())
                if let state = event.data?.eventState {
                    Text(state)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                Spacer()
                if let occurred = ControlPlaneDate.localTimeString(event.occurredAt) {
                    Text(occurred)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            if let message = event.data?.message {
                Text(message)
                    .font(.caption)
                    .textSelection(.enabled)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(6)
    }

    // MARK: - Artifacts

    private func artifactsSection(_ run: ControlPlaneRun) -> some View {
        GroupBox("Artifacts") {
            let artifacts = run.artifacts ?? []
            if artifacts.isEmpty {
                Text("No artifacts recorded")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(artifacts) { artifact in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(artifact.kind)
                                .font(.caption.bold())
                            Text(artifact.uri)
                                .font(.caption.monospaced())
                                .foregroundColor(.secondary)
                                .textSelection(.enabled)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - Raw JSON inspector

    /// The review's `evidence` blob is Homeboy core's raw, untyped JSON for
    /// this run (aggregate, execution states, promotion candidates, etc.),
    /// already fetched into `MissionStore.selectedRunReview` alongside the
    /// typed resource. Reusing it here avoids re-implementing envelope
    /// unwrapping just to show raw JSON.
    private var rawJSONSection: some View {
        DisclosureGroup("Raw JSON", isExpanded: $showRawJSON) {
            if let evidence = missionStore.selectedRunReview?.evidence {
                CopyableTextView(
                    console: evidence.prettyPrintedJSONString,
                    source: "Run Evidence",
                    maxHeight: 320
                )
            } else if missionStore.isLoadingReview {
                ProgressView("Loading raw evidence...")
            } else {
                Text("No raw evidence available")
                    .foregroundColor(.secondary)
            }
        }
    }

    // MARK: - Lifecycle

    private func activate() {
        if missionStore.missions.isEmpty {
            Task { await missionStore.loadMissions() }
        }
        if missionStore.runsById.isEmpty {
            Task { await missionStore.loadRuns() }
        }

        if let pendingRunId = navigationState.consumePendingRunSelection() {
            missionStore.selectRun(pendingRunId)
        } else {
            missionStore.resumePollingIfNeeded()
        }
    }

    private func deactivate() {
        missionStore.stopPolling()
    }
}

#Preview {
    MissionsView()
        .environmentObject(MissionStore())
        .environmentObject(AppNavigationState())
}
