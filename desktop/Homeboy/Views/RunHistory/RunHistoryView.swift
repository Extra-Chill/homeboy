import SwiftUI

struct RunHistoryView: View {
    @StateObject private var viewModel = RunHistoryViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            if viewModel.isLoadingRuns && viewModel.runs.isEmpty {
                ProgressView("Loading run history...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if viewModel.runs.isEmpty {
                emptyState
            } else {
                HSplitView {
                    runTable
                        .frame(minWidth: 680, idealWidth: 820)
                    detailPane
                        .frame(minWidth: 440)
                }
            }
        }
        .frame(minWidth: 1120, minHeight: 680)
        .onAppear {
            if viewModel.runs.isEmpty {
                viewModel.loadRuns()
            }
        }
    }

    private var header: some View {
        VStack(spacing: 12) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Run History")
                        .font(.title2.bold())
                    Text("Read-only view of persisted Homeboy CLI run records")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                Spacer()

                if viewModel.isLoadingRuns {
                    ProgressView()
                        .controlSize(.small)
                    Text("Loading...")
                        .foregroundColor(.secondary)
                }

                Button {
                    viewModel.loadRuns()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
                .disabled(viewModel.isLoadingRuns)
            }

            filters

            if let error = viewModel.error {
                InlineErrorView(error)
            }
        }
        .padding()
    }

    private var filters: some View {
        HStack(spacing: 10) {
            filterField("Kind", text: $viewModel.kindFilter, width: 100)
            filterField("Status", text: $viewModel.statusFilter, width: 100)
            filterField("Component", text: $viewModel.componentFilter, width: 140)
            filterField("Rig", text: $viewModel.rigFilter, width: 120)

            Stepper(value: $viewModel.limit, in: 1...500, step: 25) {
                Text("Limit: \(viewModel.limit)")
                    .monospacedDigit()
            }
            .frame(width: 130)

            Button("Apply") {
                viewModel.applyFilters()
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.isLoadingRuns)

            Button("Reset") {
                viewModel.resetFilters()
            }
            .buttonStyle(.bordered)
            .disabled(!viewModel.hasActiveFilters || viewModel.isLoadingRuns)

            Spacer()
        }
    }

    private func filterField(_ title: String, text: Binding<String>, width: CGFloat) -> some View {
        TextField(title, text: text)
            .textFieldStyle(.roundedBorder)
            .frame(width: width)
            .onSubmit {
                viewModel.applyFilters()
            }
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "clock.arrow.circlepath")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            Text("No Runs Found")
                .font(.headline)
            Text(viewModel.hasActiveFilters ? "No persisted runs match the current filters." : "Homeboy has not recorded any persisted runs yet.")
                .foregroundColor(.secondary)
            if viewModel.hasActiveFilters {
                Button("Reset Filters") {
                    viewModel.resetFilters()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var runTable: some View {
        Table(viewModel.runs, selection: $viewModel.selectedRunID) {
            TableColumn("Status") { run in
                statusBadge(run.status)
            }
            .width(min: 88, ideal: 100)

            TableColumn("Kind") { run in
                Text(run.kind)
                    .font(.body.monospaced())
            }
            .width(min: 70, ideal: 90)

            TableColumn("Started") { run in
                Text(formatTimestamp(run.startedAt))
            }
            .width(min: 150, ideal: 170)

            TableColumn("Finished / Duration") { run in
                VStack(alignment: .leading, spacing: 2) {
                    Text(formatTimestamp(run.finishedAt))
                    if let duration = durationText(started: run.startedAt, finished: run.finishedAt) {
                        Text(duration)
                            .font(.caption.monospacedDigit())
                            .foregroundColor(.secondary)
                    }
                }
            }
            .width(min: 150, ideal: 180)

            TableColumn("Component") { run in
                secondaryText(run.componentId)
            }
            .width(min: 110, ideal: 140)

            TableColumn("Rig") { run in
                secondaryText(run.rigId)
            }
            .width(min: 90, ideal: 120)

            TableColumn("Git SHA") { run in
                Text(shortSHA(run.gitSha))
                    .font(.body.monospaced())
                    .textSelection(.enabled)
            }
            .width(min: 90, ideal: 110)

            TableColumn("Command") { run in
                Text(run.command ?? "-")
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
            .width(min: 220, ideal: 320)
        }
        .onChange(of: viewModel.selectedRunID) { newValue in
            viewModel.selectRun(newValue)
        }
    }

    private var detailPane: some View {
        VStack(spacing: 0) {
            if viewModel.isLoadingDetail {
                ProgressView("Loading run detail...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let detail = viewModel.selectedRunDetail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        detailHeader(detail)
                        summarySection(detail)
                        metadataSection(detail)
                        artifactsSection(detail.artifacts)
                        findingsSection(viewModel.findings)
                    }
                    .padding()
                }
            } else {
                ContentUnavailableView(
                    "Select a Run",
                    systemImage: "clock.arrow.circlepath",
                    description: Text("Choose a run from the table to inspect metadata, artifacts, and findings")
                )
            }
        }
    }

    private func detailHeader(_ run: RunDetail) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                statusBadge(run.status)
                Text(run.kind)
                    .font(.headline.monospaced())
                Spacer()
            }
            Text(run.id)
                .font(.title3.bold())
                .textSelection(.enabled)
            if let note = run.statusNote, !note.isEmpty {
                Text(note)
                    .foregroundColor(.secondary)
                    .textSelection(.enabled)
            }
        }
    }

    private func summarySection(_ run: RunDetail) -> some View {
        GroupBox("Summary") {
            VStack(alignment: .leading, spacing: 8) {
                summaryRow("Started", formatTimestamp(run.startedAt))
                summaryRow("Finished", formatTimestamp(run.finishedAt))
                summaryRow("Duration", durationText(started: run.startedAt, finished: run.finishedAt) ?? "-")
                summaryRow("Component", run.componentId ?? "-")
                summaryRow("Rig", run.rigId ?? "-")
                summaryRow("Git SHA", run.gitSha ?? "-")
                summaryRow("CWD", run.cwd ?? "-")
                summaryRow("Homeboy", run.homeboyVersion ?? "-")
                summaryRow("Command", run.command ?? "-")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func metadataSection(_ run: RunDetail) -> some View {
        GroupBox("Metadata JSON") {
            CopyableTextView(
                console: run.metadata.prettyPrintedJSONString,
                source: "Run Metadata: \(run.id)",
                maxHeight: 220
            )
        }
    }

    private func artifactsSection(_ artifacts: [RunArtifact]) -> some View {
        GroupBox("Artifacts") {
            if artifacts.isEmpty {
                Text("No artifacts recorded")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(artifacts) { artifact in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(artifact.kind)
                                    .font(.caption.bold())
                                Text(artifact.artifactType)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                                Spacer()
                                if let size = artifact.sizeBytes {
                                    Text(formatBytes(size))
                                        .font(.caption.monospacedDigit())
                                        .foregroundColor(.secondary)
                                }
                            }
                            Text(artifact.path)
                                .font(.caption.monospaced())
                                .textSelection(.enabled)
                            if let url = artifact.url, !url.isEmpty {
                                Text(url)
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                                    .textSelection(.enabled)
                            }
                        }
                        .padding(8)
                        .background(Color(nsColor: .controlBackgroundColor))
                        .cornerRadius(6)
                    }
                }
            }
        }
    }

    private func findingsSection(_ findings: [RunFinding]) -> some View {
        GroupBox("Findings") {
            if findings.isEmpty {
                Text("No findings recorded")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(findings) { finding in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(finding.tool)
                                    .font(.caption.bold())
                                if let severity = finding.severity, !severity.isEmpty {
                                    Text(severity)
                                        .font(.caption.monospaced())
                                        .foregroundColor(severityColor(severity))
                                }
                                if finding.fixable == true {
                                    Text("fixable")
                                        .font(.caption.monospaced())
                                        .foregroundColor(.blue)
                                }
                                Spacer()
                            }
                            Text(finding.message)
                                .textSelection(.enabled)
                            if let file = finding.file, !file.isEmpty {
                                Text([file, finding.line.map { String($0) }].compactMap { $0 }.joined(separator: ":"))
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                                    .textSelection(.enabled)
                            }
                            if let rule = finding.rule, !rule.isEmpty {
                                Text(rule)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                            }
                        }
                        .padding(8)
                        .background(Color(nsColor: .controlBackgroundColor))
                        .cornerRadius(6)
                    }
                }
            }
        }
    }

    private func summaryRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.caption.bold())
                .foregroundColor(.secondary)
                .frame(width: 82, alignment: .leading)
            Text(value)
                .textSelection(.enabled)
            Spacer()
        }
    }

    private func statusBadge(_ status: String) -> some View {
        Label(status, systemImage: statusIcon(status))
            .font(.caption.bold())
            .foregroundColor(statusColor(status))
    }

    private func secondaryText(_ value: String?) -> some View {
        Text(value?.isEmpty == false ? value! : "-")
            .foregroundColor(value?.isEmpty == false ? .primary : .secondary)
            .textSelection(.enabled)
    }

    private func shortSHA(_ value: String?) -> String {
        guard let value, !value.isEmpty else { return "-" }
        return String(value.prefix(8))
    }

    private func formatTimestamp(_ value: String?) -> String {
        guard let value, !value.isEmpty else { return "-" }
        guard let date = parseDate(value) else { return value }

        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .medium
        return formatter.string(from: date)
    }

    private func durationText(started: String, finished: String?) -> String? {
        guard let start = parseDate(started), let finished, let end = parseDate(finished) else { return nil }
        let seconds = max(0, Int(end.timeIntervalSince(start)))
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m \(seconds % 60)s" }
        return "\(seconds / 3600)h \((seconds % 3600) / 60)m"
    }

    private func parseDate(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: value) {
            return date
        }

        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: value)
    }

    private func formatBytes(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private func statusIcon(_ status: String) -> String {
        switch status.lowercased() {
        case "success", "passed", "pass": return "checkmark.circle.fill"
        case "failed", "failure", "error": return "xmark.octagon.fill"
        case "running", "started": return "play.circle.fill"
        default: return "circle.fill"
        }
    }

    private func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "success", "passed", "pass": return .green
        case "failed", "failure", "error": return .red
        case "running", "started": return .blue
        default: return .secondary
        }
    }

    private func severityColor(_ severity: String) -> Color {
        switch severity.lowercased() {
        case "error", "critical", "high": return .red
        case "warning", "medium": return .orange
        case "low", "info": return .secondary
        default: return .primary
        }
    }
}

#Preview {
    RunHistoryView()
}
