import SwiftUI

struct HomeboyLabView: View {
    @StateObject private var viewModel = HomeboyLabViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HSplitView {
                sidebar
                    .frame(minWidth: 320, idealWidth: 380)
                detail
                    .frame(minWidth: 720)
            }
        }
        .frame(minWidth: 1120, minHeight: 700)
        .onAppear {
            viewModel.loadInitialState()
        }
        .onChange(of: viewModel.selectedRunnerID) { _, _ in
            viewModel.selectedRunnerDidChange()
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Homeboy Lab")
                        .font(.title2.bold())
                    Text("Runner cockpit backed by Homeboy runner, daemon job, and artifact contracts")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                Spacer()

                if viewModel.isLoadingRunners || viewModel.isRunningDoctor || viewModel.isLoadingJobs {
                    ProgressView()
                        .controlSize(.small)
                }

                Button {
                    viewModel.loadRunners()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
            }

            if let warning = viewModel.warning {
                InlineWarningView(warning)
            }
            if let error = viewModel.error {
                InlineErrorView(error)
            }
        }
        .padding()
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 16) {
            GroupBox("Runner") {
                VStack(alignment: .leading, spacing: 10) {
                    Picker("Execution target", selection: $viewModel.selectedRunnerID) {
                        ForEach(viewModel.runners) { runner in
                            Text(runnerLabel(runner))
                                .tag(runner.id)
                        }
                    }
                    .pickerStyle(.menu)

                    if let runner = viewModel.selectedRunner {
                        VStack(alignment: .leading, spacing: 6) {
                            labelRow("Kind", runner.kind)
                            labelRow("Workspace", runner.workspaceRoot ?? "default")
                            labelRow("Homeboy", runner.homeboyPath ?? "PATH")
                            if let limit = runner.concurrencyLimit {
                                labelRow("Concurrency", String(limit))
                            }
                            labelRow("Daemon", runner.daemon ? "preferred" : "manual")
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            GroupBox("Connection") {
                VStack(alignment: .leading, spacing: 10) {
                    statusLine(
                        title: viewModel.isLocalSelected ? "Local MacBook" : "Daemon tunnel",
                        status: viewModel.isLocalSelected ? "default" : (viewModel.isConnected ? "connected" : "disconnected")
                    )

                    if let url = viewModel.daemonURL {
                        labelRow("Daemon URL", url)
                    }
                    if let message = viewModel.connection?.failureMessage {
                        Text(message)
                            .font(.caption)
                            .foregroundColor(.red)
                            .textSelection(.enabled)
                    }

                    HStack {
                        Button("Doctor") {
                            Task { await viewModel.runDoctor() }
                        }
                        .disabled(viewModel.isRunningDoctor)

                        if !viewModel.isLocalSelected {
                            Button(viewModel.isConnected ? "Disconnect" : "Connect") {
                                if viewModel.isConnected {
                                    viewModel.disconnectSelectedRunner()
                                } else {
                                    viewModel.connectSelectedRunner()
                                }
                            }
                            .disabled(viewModel.isConnecting)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            GroupBox("Start Job") {
                VStack(alignment: .leading, spacing: 10) {
                    Picker("Action", selection: $viewModel.selectedAction) {
                        ForEach(viewModel.actions, id: \.self) { action in
                            Text(action).tag(action)
                        }
                    }

                    Picker("Component", selection: $viewModel.selectedComponentID) {
                        ForEach(viewModel.components) { component in
                            Text(component.id).tag(Optional(component.id))
                        }
                    }

                    TextField("Extra args", text: $viewModel.extraArguments)
                        .textFieldStyle(.roundedBorder)
                        .help("Passed as the daemon job body args array. Mutating flags are rejected by Homeboy core.")

                    Button {
                        viewModel.startRemoteJob()
                    } label: {
                        Label("Queue on Runner", systemImage: "play.circle")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!viewModel.canStartRemoteJob)

                    if viewModel.isLocalSelected {
                        Text("Local execution remains the default. Use the existing Quality, Bench, Rigs, and Run History tools for local runs.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    } else if !viewModel.isConnected {
                        Text("Connect this runner before queuing daemon-backed jobs.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            Spacer()
        }
        .padding()
    }

    private var detail: some View {
        VSplitView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    dependencyNote
                    doctorSection
                    jobsSection
                }
                .padding()
            }
            jobDetailSection
                .frame(minHeight: 260, idealHeight: 320)
        }
    }

    private var dependencyNote: some View {
        GroupBox("Runner Execution Contract") {
            Text("Desktop uses merged Homeboy runner registry, doctor, connect/status, daemon jobs, and artifact APIs. First-class `homeboy <command> --runner` execution is still tracked in Extra-Chill/homeboy#2529, so this cockpit queues daemon job-ready endpoints where a connected runner exposes them and leaves transport semantics to Homeboy core.")
                .font(.caption)
                .foregroundColor(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        }
    }

    private var doctorSection: some View {
        GroupBox("Readiness") {
            if let report = viewModel.doctorReport {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(spacing: 12) {
                        statusBadge(report.status)
                        labelRow("Homeboy", report.resources.homeboy.version)
                        labelRow("System", "\(report.resources.system.os) / \(report.resources.system.arch)")
                        labelRow("CPU", "\(report.resources.cpu.count) cores")
                        if let memory = report.resources.memory {
                            labelRow("Memory", "\(memory.totalMb) MB")
                        }
                    }

                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 120), spacing: 8)], alignment: .leading, spacing: 8) {
                        capabilityChip("Git", report.capabilities.git)
                        capabilityChip("GitHub CLI", report.capabilities.githubCli)
                        capabilityChip("Node", report.capabilities.node)
                        capabilityChip("PHP", report.capabilities.php)
                        capabilityChip("Docker", report.capabilities.docker)
                        capabilityChip("Playwright", report.capabilities.playwright)
                        capabilityChip("Browser", report.capabilities.browserReady)
                        capabilityChip("Workspace", report.capabilities.workspaceWritable)
                        capabilityChip("Artifacts", report.capabilities.artifactStoreAvailable)
                    }

                    Divider()

                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(report.checks) { check in
                            VStack(alignment: .leading, spacing: 3) {
                                HStack {
                                    statusBadge(check.status)
                                    Text(check.id)
                                        .font(.caption.monospaced())
                                    Text(check.message)
                                        .font(.caption)
                                    Spacer()
                                }
                                if let remediation = check.remediation, !remediation.isEmpty {
                                    Text(remediation)
                                        .font(.caption2)
                                        .foregroundColor(.secondary)
                                }
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text("Run runner doctor to inspect readiness checks.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var jobsSection: some View {
        GroupBox("Active Jobs") {
            if viewModel.isLocalSelected {
                Text("Local jobs are shown in Run History. Remote daemon jobs appear here after a Lab runner is connected.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if !viewModel.isConnected {
                Text("No daemon connection for selected runner.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else if viewModel.jobs.isEmpty {
                Text("No daemon jobs reported.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Table(viewModel.jobs, selection: $viewModel.selectedJobID) {
                    TableColumn("Status") { job in statusBadge(job.status) }
                        .width(min: 90, ideal: 110)
                    TableColumn("Operation") { job in Text(job.operation).font(.body.monospaced()) }
                        .width(min: 160, ideal: 220)
                    TableColumn("Updated") { job in Text(formatMillis(job.updatedAtMs)) }
                        .width(min: 150, ideal: 180)
                    TableColumn("Events") { job in Text(String(job.eventCount)).monospacedDigit() }
                        .width(min: 60, ideal: 80)
                    TableColumn("ID") { job in
                        Text(job.id)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                    }
                }
                .frame(minHeight: 220)
                .onChange(of: viewModel.selectedJobID) { _, id in
                    Task { await viewModel.selectJob(id) }
                }
            }
        }
    }

    private var jobDetailSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Job Events & Artifacts")
                    .font(.headline)
                Spacer()
                Button("Refresh Jobs") {
                    Task { await viewModel.refreshJobs() }
                }
                .disabled(viewModel.isLoadingJobs || !viewModel.isConnected)
                Button("Cancel") {
                    viewModel.cancelSelectedJob()
                }
                .disabled(viewModel.selectedJob == nil || viewModel.selectedJob.map { ["succeeded", "failed", "cancelled"].contains($0.status) } == true)
            }

            if let job = viewModel.selectedJob {
                HStack(spacing: 12) {
                    statusBadge(job.status)
                    Text(job.id)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer()
                }
            }

            HSplitView {
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(viewModel.events) { event in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack {
                                    Text("#\(event.sequence)")
                                        .font(.caption.monospacedDigit())
                                        .foregroundColor(.secondary)
                                    Text(event.kind)
                                        .font(.caption.bold())
                                    Text(formatMillis(event.timestampMs))
                                        .font(.caption2.monospacedDigit())
                                        .foregroundColor(.secondary)
                                }
                                if let message = event.message {
                                    Text(message)
                                        .font(.caption.monospaced())
                                        .textSelection(.enabled)
                                }
                                if let data = event.data {
                                    Text(data.prettyPrintedJSONString)
                                        .font(.caption2.monospaced())
                                        .foregroundColor(.secondary)
                                        .textSelection(.enabled)
                                }
                            }
                            .padding(6)
                            .background(Color(nsColor: .controlBackgroundColor))
                            .cornerRadius(6)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 4)
                }

                artifactsPane
                    .frame(minWidth: 260, idealWidth: 320)
            }
        }
        .padding()
    }

    private var artifactsPane: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Artifacts")
                .font(.subheadline.bold())
            if viewModel.artifacts.isEmpty {
                Text("No downloadable artifact manifest for the selected job yet.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            } else {
                ForEach(viewModel.artifacts) { artifact in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(artifact.kind)
                                .font(.caption.bold())
                            Spacer()
                            if let size = artifact.sizeBytes {
                                Text(formatBytes(size))
                                    .font(.caption2.monospacedDigit())
                                    .foregroundColor(.secondary)
                            }
                        }
                        Text(artifact.downloadPath)
                            .font(.caption2.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                        Button("Download & Open") {
                            viewModel.downloadAndOpenArtifact(artifact)
                        }
                        .disabled(viewModel.isLoadingArtifacts)
                    }
                    .padding(8)
                    .background(Color(nsColor: .controlBackgroundColor))
                    .cornerRadius(6)
                }
            }
            Spacer()
        }
        .padding(.leading, 8)
    }

    private func runnerLabel(_ runner: HomeboyRunner) -> String {
        if runner.id == HomeboyRunner.localDefault.id {
            return "Local MacBook"
        }
        return "\(runner.id) (\(runner.kind))"
    }

    private func labelRow(_ title: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(title)
                .font(.caption.bold())
                .foregroundColor(.secondary)
            Text(value)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
        }
    }

    private func statusLine(title: String, status: String) -> some View {
        HStack {
            Text(title)
                .font(.caption.bold())
            Spacer()
            statusBadge(status)
        }
    }

    private func capabilityChip(_ title: String, _ available: Bool) -> some View {
        Label(title, systemImage: available ? "checkmark.circle.fill" : "xmark.circle")
            .font(.caption)
            .foregroundColor(available ? .green : .secondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Color(nsColor: .controlBackgroundColor))
            .cornerRadius(10)
    }

    private func statusBadge(_ status: String) -> some View {
        Text(status.uppercased())
            .font(.caption2.bold())
            .foregroundColor(.white)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(statusColor(status))
            .cornerRadius(6)
    }

    private func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "ok", "succeeded", "connected", "default": return .green
        case "warn", "warning", "queued", "running": return .orange
        case "failed", "error", "cancelled", "disconnected": return .red
        default: return .secondary
        }
    }

    private func formatMillis(_ value: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(value) / 1000)
        return date.formatted(date: .abbreviated, time: .standard)
    }

    private func formatBytes(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}

#Preview {
    HomeboyLabView()
}
