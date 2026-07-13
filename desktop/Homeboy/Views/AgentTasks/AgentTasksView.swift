import SwiftUI

struct AgentTasksView: View {
    @StateObject private var viewModel = AgentTasksViewModel()

    var body: some View {
        HSplitView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    missionForm
                    runs
                }
                .padding()
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    runDetail
                    promotion
                }
                .padding()
            }
            .frame(minWidth: 540)
        }
        .frame(minWidth: 1120, minHeight: 700)
        .navigationTitle("Agent Tasks")
        .task { viewModel.load() }
        .onChange(of: viewModel.selectedRunnerID) { _, _ in Task { await viewModel.refresh() } }
        .onChange(of: viewModel.selectedRunID) { _, _ in Task { await viewModel.loadSelectedRun() } }
    }

    private var missionForm: some View {
        GroupBox("New Mission") {
            VStack(alignment: .leading, spacing: 10) {
                Picker("Lab runner", selection: $viewModel.selectedRunnerID) {
                    ForEach(viewModel.runners) { runner in Text(runner.id).tag(runner.id) }
                }
                if let runner = viewModel.selectedRunner {
                    Text("\(runner.kind) runner\(runner.concurrencyLimit.map { " · capacity \($0)" } ?? "")")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                TextField("Goal", text: $viewModel.goal, axis: .vertical)
                TextField("Source refs (one per line)", text: $viewModel.sourceRefs, axis: .vertical)
                TextField("Workspace", text: $viewModel.workspace)
                TextField("Provider backend", text: $viewModel.provider)
                HStack {
                    Stepper("Concurrency: \(viewModel.concurrency)", value: $viewModel.concurrency, in: 1...32)
                    Stepper("Attempts: \(viewModel.attempts)", value: $viewModel.attempts, in: 1...10)
                }
                TextField("Policy JSON", text: $viewModel.policy, axis: .vertical)
                    .font(.system(.caption, design: .monospaced))
                Toggle("Run immediately after durable submission", isOn: $viewModel.runNow)
                Button(viewModel.runNow ? "Submit & Run" : "Queue Mission") { viewModel.submit() }
                    .buttonStyle(.borderedProminent)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var runs: some View {
        GroupBox("Durable Runs") {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Homeboy lifecycle state")
                    Spacer()
                    Button { Task { await viewModel.refresh() } } label: { Image(systemName: "arrow.clockwise") }
                }
                ForEach(0..<viewModel.runs.count, id: \.self) { index in
                    let run = viewModel.runs[index]
                    Button {
                        viewModel.selectedRunID = run.value(at: ["run_id"]).stringValue
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(run.value(at: ["run_id"]).stringValue)
                                .font(.caption.monospaced())
                            Text("\(run.value(at: ["state"]).stringValue)  \(run.value(at: ["counts"]).stringValue)")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.plain)
                    .padding(7)
                    .background(viewModel.selectedRunID == run.value(at: ["run_id"]).stringValue ? Color.accentColor.opacity(0.14) : Color(nsColor: .controlBackgroundColor))
                    .cornerRadius(6)
                }
            }
        }
    }

    private var runDetail: some View {
        GroupBox("Mission Control") {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Button("Cancel") { viewModel.cancel() }
                    Button("Retry") { viewModel.retry() }
                    Button("Resume") { viewModel.resume() }
                    Spacer()
                    if viewModel.isLoading { ProgressView() }
                }
                contractSection("Status / Fanout", viewModel.status)
                contractSection("Events / Attempts / Failures", viewModel.logs)
                contractSection("Artifacts / Evidence / Replay / PR", viewModel.artifacts)
                if let error = viewModel.error { InlineErrorView(error) }
            }
        }
    }

    private var promotion: some View {
        GroupBox("Promotion Preview") {
            VStack(alignment: .leading, spacing: 8) {
                Text("Dry-runs Homeboy promotion from the selected durable run. No patch is applied from Desktop.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                TextField("Managed worktree handle", text: $viewModel.promotionWorktree)
                TextField("Verification command", text: $viewModel.verificationCommand)
                Button("Dry-run Promotion") { viewModel.dryRunPromotion() }
                if !viewModel.promotionResult.isEmpty {
                    Text(viewModel.promotionResult)
                        .font(.system(.caption2, design: .monospaced))
                        .textSelection(.enabled)
                }
            }
        }
    }

    private func contractSection(_ title: String, _ value: JSONValue) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.headline)
            Text(value.prettyPrintedJSONString)
                .font(.system(.caption2, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(Color(nsColor: .textBackgroundColor))
                .cornerRadius(6)
        }
    }
}
