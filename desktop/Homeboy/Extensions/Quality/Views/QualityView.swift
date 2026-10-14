import SwiftUI

struct QualityView: View {
    @StateObject private var viewModel = QualityViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            if viewModel.components.isEmpty {
                ContentUnavailableView(
                    "No Components",
                    systemImage: "shippingbox",
                    description: Text("Add a component before running Homeboy quality checks.")
                )
            } else {
                HSplitView {
                    summaryPane
                        .frame(minWidth: 460)
                    consolePane
                        .frame(minWidth: 340)
                }
            }
        }
        .frame(minWidth: 900, minHeight: 600)
        .onAppear { viewModel.refreshComponents() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Label("Quality", systemImage: "checkmark.seal")
                    .font(.title2.bold())

                Spacer()

                if viewModel.isBusy {
                    ProgressView()
                        .controlSize(.small)
                }
            }

            HStack(spacing: 12) {
                Picker("Component", selection: Binding(
                    get: { viewModel.selectedComponentId ?? "" },
                    set: { viewModel.selectedComponentId = $0 }
                )) {
                    ForEach(viewModel.components) { component in
                        Text(component.id).tag(component.id)
                    }
                }
                .frame(minWidth: 220)

                Picker("Scope", selection: $viewModel.scope) {
                    ForEach(QualityScope.allCases) { scope in
                        Text(scope.label).tag(scope)
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 300)

                if viewModel.scope == .changedSince {
                    TextField("Git ref", text: $viewModel.changedSince)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 160)
                }

                Spacer()

                Button("Run Review") { viewModel.runReview() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!viewModel.canRun)
            }
        }
        .padding()
    }

    private var summaryPane: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let error = viewModel.error {
                    InlineErrorView(error)
                }

                if let review = viewModel.review {
                    reviewSummary(review)
                    stageGrid(review)
                } else {
                    emptyReview
                }
            }
            .padding()
        }
    }

    private var emptyReview: some View {
        ContentUnavailableView(
            "Run Review",
            systemImage: "doc.text.magnifyingglass",
            description: Text("Start with `homeboy review --summary` for a read-only lint and test overview.")
        )
        .frame(maxWidth: .infinity, minHeight: 220)
    }

    private func reviewSummary(_ review: QualityReviewOutput) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(review.summary.status.capitalized, systemImage: review.summary.passed ? "checkmark.circle.fill" : "xmark.octagon.fill")
                    .font(.headline)
                    .foregroundColor(review.summary.passed ? .green : .red)
                Spacer()
                Text(review.summary.scope)
                    .font(.caption.monospaced())
                    .foregroundColor(.secondary)
            }

            Text(review.summary.component)
                .font(.title3.bold())

            Text("\(review.summary.totalFindings) findings across lint and test stages")
                .foregroundColor(.secondary)
        }
        .padding()
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(10)
    }

    private func stageGrid(_ review: QualityReviewOutput) -> some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 12)], spacing: 12) {
            stageActionCard(.lint, result: review.lint)
            stageActionCard(.test, result: review.test)
        }
    }

    private func stageActionCard(_ stage: QualityStage, result: QualityStageResult?) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(stage.label, systemImage: stage.icon)
                    .font(.headline)
                Spacer()
                if let result {
                    Text(result.statusLabel)
                        .font(.caption.bold())
                        .foregroundColor(result.passed ? .green : .red)
                }
            }

            if let result {
                Text("Findings: \(result.findingCount ?? 0)")
                    .font(.caption)
                if let hint = result.hint {
                    Text(hint)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .lineLimit(2)
                }
            } else {
                Text("Run this stage through homeboy review.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Button(viewModel.runningStage == stage ? "Running..." : "Run \(stage.label)") {
                viewModel.runStage(stage)
            }
            .disabled(!viewModel.canRun)
        }
        .padding()
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(10)
    }

    private var consolePane: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Console")
                    .font(.headline)
                Spacer()
                CopyButton.console(viewModel.consoleOutput, source: "Quality")
                    .disabled(viewModel.consoleOutput.isEmpty)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)

            Divider()

            ScrollView {
                Text(viewModel.consoleOutput.isEmpty ? "Quality command output will appear here." : viewModel.consoleOutput)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(viewModel.consoleOutput.isEmpty ? .secondary : .primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
            }
            .background(Color(nsColor: .textBackgroundColor))
        }
    }
}
