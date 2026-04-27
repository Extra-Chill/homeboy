import SwiftUI

struct RigsView: View {
    @StateObject private var viewModel = RigsViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            if viewModel.isLoading && viewModel.rigs.isEmpty {
                ProgressView("Loading rigs...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if viewModel.rigs.isEmpty {
                emptyState
            } else {
                HSplitView {
                    rigList
                        .frame(minWidth: 280, idealWidth: 340, maxWidth: 420)
                    rigDetail
                        .frame(minWidth: 520)
                }
            }
        }
        .frame(minWidth: 900, minHeight: 620)
        .onAppear {
            if viewModel.rigs.isEmpty {
                viewModel.load()
            }
        }
        .alert(item: $viewModel.pendingMutatingAction) { action in
            Alert(
                title: Text(action.title),
                message: Text("This runs `homeboy rig \(action.rawValue) \(viewModel.selectedRigID ?? "")`. It may start or stop local services and modify development checkouts."),
                primaryButton: .destructive(Text("Run \(action.rawValue)")) {
                    viewModel.runPendingMutatingAction()
                },
                secondaryButton: .cancel()
            )
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Rigs")
                    .font(.title2.bold())
                Text("Inspect and check reproducible Homeboy development environments")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if let running = viewModel.runningCommand {
                ProgressView()
                    .controlSize(.small)
                Text("Running \(running)...")
                    .foregroundColor(.secondary)
            }

            Button {
                viewModel.load()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.bordered)
            .disabled(viewModel.isRunningCommand)
        }
        .padding()
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "shippingbox")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            Text("No Rigs Found")
                .font(.headline)
            Text("Create rigs with the Homeboy CLI, then return here to inspect and check them.")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var rigList: some View {
        List(selection: $viewModel.selectedRigID) {
            ForEach(viewModel.rigs) { rig in
                Button {
                    viewModel.selectRig(rig.id)
                } label: {
                    RigRow(rig: rig)
                }
                .buttonStyle(.plain)
                .tag(rig.id)
            }
        }
        .listStyle(.sidebar)
    }

    private var rigDetail: some View {
        VStack(spacing: 0) {
            if let rig = viewModel.selectedRig {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        selectedRigHeader(rig)

                        if let error = viewModel.error {
                            InlineErrorView(error)
                        }

                        actionBar
                        statusSection
                        checkSection
                        specSection(rig)
                        consoleSection
                    }
                    .padding()
                }
            } else {
                ContentUnavailableView(
                    "Select a Rig",
                    systemImage: "shippingbox",
                    description: Text("Choose a rig from the sidebar to inspect it")
                )
            }
        }
    }

    private func selectedRigHeader(_ rig: RigSpec) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(rig.id)
                .font(.title.bold())
                .textSelection(.enabled)
            if let description = rig.description, !description.isEmpty {
                Text(description)
                    .foregroundColor(.secondary)
                    .textSelection(.enabled)
            }
        }
    }

    private var actionBar: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Read-only")
                .font(.caption.bold())
                .foregroundColor(.secondary)
            HStack {
                Button {
                    viewModel.runStatus()
                } label: {
                    Label("Status", systemImage: "list.bullet.rectangle")
                }
                .buttonStyle(.bordered)

                Button {
                    viewModel.runCheck()
                } label: {
                    Label("Check", systemImage: "checkmark.seal")
                }
                .buttonStyle(.borderedProminent)

                Divider()

                Text("Lifecycle")
                    .font(.caption.bold())
                    .foregroundColor(.secondary)

                Button("Up") {
                    viewModel.confirmMutatingAction(.up)
                }
                .buttonStyle(.bordered)

                Button("Down") {
                    viewModel.confirmMutatingAction(.down)
                }
                .buttonStyle(.bordered)

                Spacer()

                Button("Clear Output") {
                    viewModel.clearConsole()
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.consoleOutput.isEmpty)
            }
            .disabled(viewModel.isRunningCommand)
        }
        .padding()
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private var statusSection: some View {
        GroupBox("Status") {
            if let status = viewModel.status {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 12) {
                        infoPill(label: "Last check", value: status.lastCheckResult ?? "unknown")
                        infoPill(label: "Checked", value: status.lastCheck ?? "never")
                        infoPill(label: "Last up", value: status.lastUp ?? "never")
                    }

                    if status.services.isEmpty {
                        Text("No services declared")
                            .foregroundColor(.secondary)
                    } else {
                        ForEach(status.services) { service in
                            HStack {
                                Image(systemName: service.status == "running" ? "checkmark.circle.fill" : "circle")
                                    .foregroundColor(service.status == "running" ? .green : .secondary)
                                Text(service.id)
                                    .font(.body.monospaced())
                                Text(service.kind)
                                    .foregroundColor(.secondary)
                                Spacer()
                                Text(service.status)
                                    .font(.caption.bold())
                                if let port = service.port {
                                    Text(":\(port)")
                                        .font(.caption.monospaced())
                                        .foregroundColor(.secondary)
                                }
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text("Run Status to load service state.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var checkSection: some View {
        GroupBox("Latest Check") {
            if let check = viewModel.checkResult {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Image(systemName: check.success ? "checkmark.circle.fill" : "xmark.octagon.fill")
                            .foregroundColor(check.success ? .green : .red)
                        Text(check.success ? "Passed" : "Failed")
                            .font(.headline)
                        Spacer()
                        Text("\(check.pipeline.passed ?? 0) passed / \(check.pipeline.failed ?? 0) failed")
                            .foregroundColor(.secondary)
                    }

                    ForEach(check.pipeline.steps) { step in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Image(systemName: step.status == "pass" ? "checkmark.circle" : "xmark.circle")
                                    .foregroundColor(step.status == "pass" ? .green : .red)
                                Text(step.label)
                                Spacer()
                                Text(step.kind)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                            }
                            if let error = step.error {
                                Text(error)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.red)
                                    .textSelection(.enabled)
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text("Run Check to validate this rig.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func specSection(_ rig: RigSpec) -> some View {
        GroupBox("Spec") {
            VStack(alignment: .leading, spacing: 12) {
                specSummary(rig)

                if !rig.components.isEmpty {
                    Text("Components")
                        .font(.headline)
                    ForEach(rig.components.keys.sorted(), id: \.self) { id in
                        if let component = rig.components[id] {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(id)
                                    .font(.body.monospaced().bold())
                                Text(component.path)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                                    .textSelection(.enabled)
                                if let branch = component.branch {
                                    Text("branch: \(branch)")
                                        .font(.caption)
                                        .foregroundColor(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func specSummary(_ rig: RigSpec) -> some View {
        HStack(spacing: 12) {
            infoPill(label: "Components", value: "\(rig.components.count)")
            infoPill(label: "Services", value: "\(rig.services?.count ?? 0)")
            infoPill(label: "Symlinks", value: "\(rig.symlinks?.count ?? 0)")
            infoPill(label: "Pipelines", value: rig.pipeline?.keys.sorted().joined(separator: ", ") ?? "none")
        }
    }

    private var consoleSection: some View {
        GroupBox("Command Output") {
            if viewModel.consoleOutput.isEmpty {
                Text("Command output will appear here and can be copied.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                CopyableTextView(console: viewModel.consoleOutput, source: "Rigs", maxHeight: 280)
            }
        }
    }

    private func infoPill(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption2.bold())
                .foregroundColor(.secondary)
            Text(value)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(Color(nsColor: .textBackgroundColor))
        .cornerRadius(6)
    }
}

private struct RigRow: View {
    let rig: RigListItem

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(rig.id)
                    .font(.headline)
                Spacer()
                if rig.source?.linked == true {
                    Image(systemName: "link")
                        .foregroundColor(.secondary)
                        .help("Installed from linked source")
                }
            }

            if let description = rig.description, !description.isEmpty {
                Text(description)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(2)
            }

            HStack(spacing: 8) {
                Text("\(rig.componentCount) components")
                Text("\(rig.serviceCount) services")
                Text(rig.pipelines.joined(separator: ", "))
            }
            .font(.caption2)
            .foregroundColor(.secondary)
        }
        .padding(.vertical, 4)
    }
}

#Preview {
    RigsView()
}
