import Combine
import SwiftUI

@MainActor
final class BenchViewModel: ObservableObject, ConfigurationObserving {
    var cancellables = Set<AnyCancellable>()

    @Published var components: [ComponentConfiguration] = []
    @Published var selectedComponentId: String = ""
    @Published var iterations: Int = 10
    @Published var scenarios: [BenchScenario] = []
    @Published var lastRun: BenchCommandOutput?
    @Published var isLoadingScenarios = false
    @Published var isRunning = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared

    init() {
        loadComponents()
        observeConfiguration()
    }

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch, .projectModified:
            loadComponents()
        default:
            break
        }
    }

    func loadComponents() {
        let project = ConfigurationManager.shared.safeActiveProject
        components = ConfigurationManager.shared.loadComponentsForProject(project)
            .sorted { $0.displayName.localizedStandardCompare($1.displayName) == .orderedAscending }

        if !components.contains(where: { $0.id == selectedComponentId }) {
            selectedComponentId = components.first?.id ?? ""
            scenarios = []
            lastRun = nil
        }
    }

    var selectedComponent: ComponentConfiguration? {
        components.first { $0.id == selectedComponentId }
    }

    var canRun: Bool {
        !selectedComponentId.isEmpty && !isRunning && iterations > 0
    }

    func loadScenarios() {
        guard let component = selectedComponent else { return }

        isLoadingScenarios = true
        error = nil

        Task {
            do {
                let output = try await cli.benchList(componentId: component.id, path: component.localPath)
                scenarios = output.results?.scenarios ?? []
                lastRun = output
            } catch {
                scenarios = []
                self.error = AppError(error.localizedDescription, source: "Bench List")
            }
            isLoadingScenarios = false
        }
    }

    func runBench() {
        guard let component = selectedComponent else { return }

        isRunning = true
        error = nil
        lastRun = nil

        Task {
            do {
                let clampedIterations = max(1, iterations)
                let output = try await cli.benchRun(
                    componentId: component.id,
                    path: component.localPath,
                    iterations: clampedIterations
                )
                lastRun = output
                scenarios = output.results?.scenarios ?? scenarios
            } catch {
                self.error = AppError(error.localizedDescription, source: "Bench")
            }
            isRunning = false
        }
    }
}

struct BenchView: View {
    @StateObject private var viewModel = BenchViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            HSplitView {
                configurationPanel
                    .frame(minWidth: 300, idealWidth: 360)
                resultsPanel
                    .frame(minWidth: 420)
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            viewModel.loadComponents()
        }
        .onChange(of: viewModel.selectedComponentId) { _, _ in
            viewModel.scenarios = []
            viewModel.lastRun = nil
        }
    }

    private var header: some View {
        HStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Benchmarks")
                    .font(.title2.weight(.semibold))
                Text("Run `homeboy bench` without writing baselines or ratchets.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if viewModel.isLoadingScenarios || viewModel.isRunning {
                ProgressView()
                    .controlSize(.small)
            }

            Button("List Scenarios") {
                viewModel.loadScenarios()
            }
            .disabled(viewModel.selectedComponentId.isEmpty || viewModel.isLoadingScenarios || viewModel.isRunning)

            Button("Run Bench") {
                viewModel.runBench()
            }
            .buttonStyle(.borderedProminent)
            .disabled(!viewModel.canRun)
        }
        .padding()
    }

    private var configurationPanel: some View {
        VStack(alignment: .leading, spacing: 16) {
            GroupBox("Component") {
                VStack(alignment: .leading, spacing: 10) {
                    if viewModel.components.isEmpty {
                        Text("No components are configured for this project.")
                            .foregroundColor(.secondary)
                    } else {
                        Picker("Component", selection: $viewModel.selectedComponentId) {
                            ForEach(viewModel.components) { component in
                                Text(component.displayName).tag(component.id)
                            }
                        }

                        if let component = viewModel.selectedComponent {
                            Text(component.localPath)
                                .font(.caption.monospaced())
                                .foregroundColor(.secondary)
                                .textSelection(.enabled)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }

            GroupBox("Run Settings") {
                Stepper(value: $viewModel.iterations, in: 1...1_000) {
                    Text("Iterations: \(viewModel.iterations)")
                }
                .padding(.vertical, 4)
            }

            GroupBox("Scenarios") {
                if viewModel.scenarios.isEmpty {
                    Text("List scenarios to preview what this component can benchmark.")
                        .foregroundColor(.secondary)
                } else {
                    List(viewModel.scenarios) { scenario in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(scenario.id)
                                .font(.body.weight(.medium))
                            if let file = scenario.file {
                                Text(file)
                                    .font(.caption.monospaced())
                                    .foregroundColor(.secondary)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .frame(minHeight: 180)
                }
            }

            Spacer()
        }
        .padding()
    }

    private var resultsPanel: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let error = viewModel.error {
                InlineErrorView(error)
            }

            if let run = viewModel.lastRun {
                runSummary(run)

                if let results = run.results {
                    scenarioResults(results.scenarios)
                }

                if let comparison = run.baselineComparison {
                    baselineSummary(comparison)
                }
            } else {
                ContentUnavailableView(
                    "No Benchmark Results",
                    systemImage: "speedometer",
                    description: Text("Choose a component, list scenarios, or run a benchmark to see timing summaries.")
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding()
    }

    private func runSummary(_ run: BenchCommandOutput) -> some View {
        GroupBox("Summary") {
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 8) {
                summaryRow("Component", run.component)
                summaryRow("Status", run.status.capitalized)
                summaryRow("Iterations", String(run.iterations))
                summaryRow("Exit Code", String(run.exitCode))
                summaryRow("Passed", run.passed ? "Yes" : "No")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func scenarioResults(_ scenarios: [BenchScenario]) -> some View {
        GroupBox("Scenario Results") {
            if scenarios.isEmpty {
                Text("No scenario results were returned.")
                    .foregroundColor(.secondary)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(scenarios) { scenario in
                            scenarioResultRow(scenario)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    private func scenarioResultRow(_ scenario: BenchScenario) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(scenario.id)
                    .font(.headline)
                Spacer()
                if let p95 = scenario.metrics["p95_ms"] {
                    Text(formatMetric(p95, name: "p95_ms"))
                        .font(.system(.body, design: .monospaced))
                }
            }

            if let file = scenario.file {
                Text(file)
                    .font(.caption.monospaced())
                    .foregroundColor(.secondary)
            }

            metricChips(for: scenario)
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private func metricChips(for scenario: BenchScenario) -> some View {
        let metrics = scenario.metrics.sorted { $0.key < $1.key }
        return VStack(alignment: .leading, spacing: 6) {
            ForEach(metrics, id: \.key) { metric in
                Text("\(metric.key): \(formatMetric(metric.value, name: metric.key))")
                    .font(.caption.monospaced())
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Color(nsColor: .quaternaryLabelColor).opacity(0.18))
                    .cornerRadius(5)
            }

            if let memory = scenario.memory {
                Text("peak: \(formatBytes(memory.peakBytes))")
                    .font(.caption.monospaced())
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Color(nsColor: .quaternaryLabelColor).opacity(0.18))
                    .cornerRadius(5)
            }
        }
    }

    private func baselineSummary(_ comparison: BenchBaselineComparison) -> some View {
        GroupBox("Baseline Comparison") {
            VStack(alignment: .leading, spacing: 8) {
                Label(
                    comparison.regression ? "Regression detected" : "No regression detected",
                    systemImage: comparison.regression ? "exclamationmark.triangle.fill" : "checkmark.circle.fill"
                )
                .foregroundColor(comparison.regression ? .orange : .green)

                Text("Threshold: \(formatNumber(comparison.thresholdPercent))%")
                    .font(.caption)
                    .foregroundColor(.secondary)

                ForEach(comparison.reasons ?? [], id: \.self) { reason in
                    Text(reason)
                        .font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func summaryRow(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label)
                .foregroundColor(.secondary)
            Text(value)
                .font(.system(.body, design: .monospaced))
        }
    }

    private func formatMetric(_ value: Double, name: String) -> String {
        if name.hasSuffix("_ms") {
            return "\(formatNumber(value)) ms"
        }
        return formatNumber(value)
    }

    private func formatNumber(_ value: Double) -> String {
        value.formatted(.number.precision(.fractionLength(0...2)))
    }

    private func formatBytes(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .memory)
    }
}
