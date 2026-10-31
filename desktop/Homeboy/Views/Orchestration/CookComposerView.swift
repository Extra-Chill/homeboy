import SwiftUI

/// "New Cook" composer, opened from a toolbar button in Missions and
/// Activity. Builds `homeboy agent-task cook` arguments from a form,
/// validates them with a non-mutating `--preview`, then submits the
/// identical arguments. Homeboy core validates, places, and executes the
/// Cook; this view only renders what core reports.
struct CookComposerView: View {
    @EnvironmentObject private var missionStore: MissionStore
    @EnvironmentObject private var navigationState: AppNavigationState
    @Environment(\.dismiss) private var dismiss

    @StateObject private var viewModel = CookComposerViewModel()

    var body: some View {
        NavigationStack {
            Form {
                if let error = viewModel.error {
                    Section {
                        InlineErrorView(error) { viewModel.error = nil }
                    }
                }
                if let formLevelError = viewModel.formLevelError {
                    Section {
                        InlineErrorView(formLevelError, source: "New Cook")
                    }
                }

                repositorySection
                trackerSection
                promptSection
                verifySection
                modelSection
                placementSection
                previewSection
            }
            .formStyle(.grouped)
            .navigationTitle("New Cook")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .frame(minWidth: 600, minHeight: 700)
        .task { await viewModel.loadContext() }
        .onChange(of: viewModel.submittedRunID) { _, newValue in
            guard let newValue else { return }
            Task {
                await missionStore.loadMissions()
                await missionStore.loadRuns()
                navigationState.openRun(newValue)
                dismiss()
            }
        }
    }

    // MARK: - Repository

    private var repositorySection: some View {
        Section("Repository") {
            HStack {
                TextField("Repository or component ID", text: $viewModel.form.repo)
                    .textFieldStyle(.roundedBorder)
                if !viewModel.componentIDs.isEmpty {
                    Menu {
                        ForEach(viewModel.componentIDs, id: \.self) { id in
                            Button(id) { viewModel.form.repo = id }
                        }
                    } label: {
                        Image(systemName: "chevron.down.circle")
                    }
                    .menuStyle(.borderlessButton)
                    .frame(width: 24)
                }
            }
            fieldError(.repo)
        }
    }

    // MARK: - Tracker / goal

    private var trackerSection: some View {
        Section("Task") {
            TextField("Tracker URL", text: $viewModel.form.taskURL)
                .textFieldStyle(.roundedBorder)
            fieldError(.taskURL)

            TextField("Goal", text: $viewModel.form.goal)
                .textFieldStyle(.roundedBorder)
            fieldError(.goal)
        }
    }

    // MARK: - Prompt

    private var promptSection: some View {
        Section("Prompt") {
            TextEditor(text: $viewModel.form.prompt)
                .frame(minHeight: 120)
                .overlay(alignment: .topLeading) {
                    if viewModel.form.prompt.isEmpty {
                        Text("Describe the work in detail. Sent over stdin, so quoting never applies.")
                            .foregroundColor(.secondary)
                            .padding(.top, 8)
                            .padding(.leading, 4)
                            .allowsHitTesting(false)
                    }
                }
            fieldError(.prompt)
        }
    }

    // MARK: - Verification gates

    private var verifySection: some View {
        Section("Verification Gates") {
            ForEach(viewModel.form.verifyGates.indices, id: \.self) { index in
                HStack {
                    TextField("Command (e.g. cargo test)", text: Binding(
                        get: { viewModel.form.verifyGates[index] },
                        set: { viewModel.form.verifyGates[index] = $0 }
                    ))
                    .textFieldStyle(.roundedBorder)

                    Button {
                        viewModel.form.verifyGates.remove(at: index)
                    } label: {
                        Image(systemName: "minus.circle")
                    }
                    .buttonStyle(.borderless)
                    .disabled(viewModel.form.verifyGates.count <= 1)
                }
            }
            Button {
                viewModel.form.verifyGates.append("")
            } label: {
                Label("Add Gate", systemImage: "plus.circle")
            }
            fieldError(.verify)
        }
    }

    // MARK: - Model

    private var modelSection: some View {
        Section("Model") {
            Picker("Model", selection: $viewModel.form.modelID) {
                Text("Use configured route").tag(String?.none)
                ForEach(viewModel.modelOptions) { option in
                    Text(option.displayName).tag(String?.some(option.id))
                }
            }
            fieldError(.model)

            if let warning = viewModel.capacityWarning {
                capacityWarningBanner(warning)
            }
        }
    }

    private func capacityWarningBanner(_ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            Text(text)
                .font(.caption)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.12))
        .cornerRadius(6)
    }

    // MARK: - Placement / publication

    private var placementSection: some View {
        Section("Placement") {
            Picker("Placement", selection: $viewModel.form.placement) {
                ForEach(CookPlacementOption.allCases) { option in
                    Text(option.label).tag(option)
                }
            }
            .pickerStyle(.segmented)
            fieldError(.placement)

            Toggle("Open a draft pull request", isOn: $viewModel.form.draftPR)
            fieldError(.draftPR)
        }
    }

    // MARK: - Preview / submit

    private var previewSection: some View {
        Section("Preview") {
            HStack {
                Button {
                    Task { await viewModel.preview() }
                } label: {
                    if viewModel.isPreviewing {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Preview")
                    }
                }
                .buttonStyle(.bordered)
                .disabled(!viewModel.canPreview)

                Button {
                    Task { await viewModel.submit() }
                } label: {
                    if viewModel.isSubmitting {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Submit")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!viewModel.canSubmit)

                Spacer()
            }

            if viewModel.isPreviewStale {
                Text("Edited since the last preview. Preview again before submitting.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            if let data = viewModel.previewData {
                previewSummary(data)
            }
        }
    }

    private func previewSummary(_ data: CookPreviewData) -> some View {
        let resolved = data.resolved
        return VStack(alignment: .leading, spacing: 6) {
            summaryRow("Placement", placementText(resolved?.placement))
            summaryRow("Provider", providerText(resolved?.provider))
            summaryRow("Destination", resolved?.workspace?.path ?? resolved?.worktree ?? "-")
            summaryRow("Branch", branchText(resolved))
            summaryRow("Gates", gatesText(resolved?.gates))
            summaryRow("Pull request", publicationText(resolved?.publication))
        }
        .padding(10)
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
                .font(.caption)
                .textSelection(.enabled)
            Spacer()
        }
    }

    private func placementText(_ placement: CookResolvedPlacement?) -> String {
        guard let selected = placement?.selected else { return "-" }
        if let requested = placement?.requested, requested != selected {
            return "\(selected) (requested \(requested))"
        }
        return selected
    }

    private func providerText(_ provider: CookResolvedProvider?) -> String {
        guard let backend = provider?.backend else { return "-" }
        if let model = provider?.model {
            return "\(backend) · \(model)"
        }
        return backend
    }

    private func branchText(_ resolved: CookResolvedSummary?) -> String {
        let head = resolved?.head ?? resolved?.workspace?.branch
        guard let head else { return "-" }
        if let base = resolved?.base {
            return "\(head) → \(base)"
        }
        return head
    }

    private func gatesText(_ gates: CookResolvedGates?) -> String {
        guard let gates else { return "-" }
        return "\(gates.total) (\(gates.publicCount ?? 0) public, \(gates.privateCount ?? 0) private)"
    }

    private func publicationText(_ publication: CookResolvedPublication?) -> String {
        guard let publication else { return "-" }
        if publication.finalize == false {
            return "Not finalized (--no-finalize)"
        }
        return publication.draft == true ? "Draft" : "Ready for review"
    }

    // MARK: - Field errors

    @ViewBuilder
    private func fieldError(_ field: CookComposerField) -> some View {
        if let message = viewModel.fieldErrors[field] {
            Text(message)
                .font(.caption)
                .foregroundColor(.red)
        }
    }
}

#Preview {
    CookComposerView()
        .environmentObject(MissionStore())
        .environmentObject(AppNavigationState())
}
