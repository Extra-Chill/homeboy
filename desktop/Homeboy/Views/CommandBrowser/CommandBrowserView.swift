import SwiftUI

struct CommandBrowserView: View {
    @StateObject private var viewModel = CommandBrowserViewModel()
    @State private var selectedCommandID: String?

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()

            HSplitView {
                commandList
                    .frame(minWidth: 300, idealWidth: 360, maxWidth: 440)
                commandDetail
                    .frame(minWidth: 620)
            }
        }
        .frame(minWidth: 960, minHeight: 640)
        .onAppear {
            selectedCommandID = viewModel.selectedCommand?.id
            viewModel.loadInitialHelp()
        }
        .onChange(of: selectedCommandID) { _, newValue in
            guard let newValue,
                  let command = viewModel.commands.first(where: { $0.id == newValue }) else { return }
            viewModel.select(command)
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Commands")
                    .font(.title2.bold())
                Text("GUI wrapper for the Homeboy CLI command surface")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if viewModel.isLoadingHelp || viewModel.isRunning {
                ProgressView()
                    .controlSize(.small)
            }
        }
        .padding()
    }

    private var commandList: some View {
        VStack(spacing: 0) {
            TextField("Filter commands", text: $viewModel.filter)
                .textFieldStyle(.roundedBorder)
                .padding()

            List(selection: $selectedCommandID) {
                ForEach(viewModel.filteredCommands) { command in
                    CommandRow(command: command)
                        .tag(command.id)
                }
            }
            .listStyle(.sidebar)
        }
    }

    private var commandDetail: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let command = viewModel.selectedCommand {
                    commandHeader(command)

                    if let error = viewModel.error {
                        InlineErrorView(error)
                    }

                    commandRunner(command)
                    helpSection
                    outputSection
                } else {
                    ContentUnavailableView(
                        "Select a Command",
                        systemImage: "terminal",
                        description: Text("Choose a Homeboy CLI command to inspect its help and run explicit invocations.")
                    )
                }
            }
            .padding()
        }
    }

    private func commandHeader(_ command: CommandBrowserEntry) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(command.invocation)
                .font(.largeTitle.bold().monospaced())
                .textSelection(.enabled)
            Text(command.summary)
                .foregroundColor(.secondary)
            HStack(spacing: 8) {
                badge(command.scope.rawValue, color: .blue)
                badge(command.risk.rawValue, color: riskColor(command.risk))
                badge(command.coverage.rawValue, color: coverageColor(command.coverage))
            }
        }
    }

    private func commandRunner(_ command: CommandBrowserEntry) -> some View {
        GroupBox("Invocation") {
            VStack(alignment: .leading, spacing: 10) {
                Text("Edit the exact CLI command before running. Existing workflow tabs remain the safer path for guided operations.")
                    .font(.caption)
                    .foregroundColor(.secondary)

                TextField("homeboy ...", text: $viewModel.commandInput)
                    .font(.system(.body, design: .monospaced))
                    .textFieldStyle(.roundedBorder)

                HStack {
                    Button {
                        viewModel.commandInput = command.invocation + " --help"
                        viewModel.loadHelp(for: command)
                    } label: {
                        Label("Load Help", systemImage: "questionmark.circle")
                    }
                    .buttonStyle(.bordered)

                    Button {
                        viewModel.copyCurrentCommand()
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    .buttonStyle(.bordered)

                    Button {
                        viewModel.runCommandInput()
                    } label: {
                        Label("Run", systemImage: "play.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(viewModel.isRunning)

                    Spacer()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var helpSection: some View {
        GroupBox("CLI Help") {
            if viewModel.helpOutput.isEmpty {
                Text("Help output will appear here.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                CopyableTextView(console: viewModel.helpOutput, source: "Commands", maxHeight: 260)
            }
        }
    }

    private var outputSection: some View {
        GroupBox("Run Output") {
            if viewModel.runOutput.isEmpty {
                Text("Run output will appear here.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                CopyableTextView(console: viewModel.runOutput, source: "Commands", maxHeight: 320)
            }
        }
    }

    private func badge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.caption.bold())
            .foregroundColor(color)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(color.opacity(0.12))
            .cornerRadius(6)
    }

    private func riskColor(_ risk: CommandBrowserEntry.Risk) -> Color {
        switch risk {
        case .readOnly: return .green
        case .guardedWrites: return .orange
        case .mutating: return .red
        case .operatorOnly: return .purple
        }
    }

    private func coverageColor(_ coverage: CommandBrowserEntry.DesktopCoverage) -> Color {
        switch coverage {
        case .workflow: return .green
        case .partial: return .orange
        case .readOnly: return .blue
        case .cliOnly: return .secondary
        }
    }
}

private struct CommandRow: View {
    let command: CommandBrowserEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(command.command)
                    .font(.headline.monospaced())
                Spacer()
                Text(command.coverage.rawValue)
                    .font(.caption2.bold())
                    .foregroundColor(.secondary)
            }

            Text(command.summary)
                .font(.caption)
                .foregroundColor(.secondary)
                .lineLimit(2)

            HStack(spacing: 8) {
                Text(command.scope.rawValue)
                Text(command.risk.rawValue)
            }
            .font(.caption2)
            .foregroundColor(.secondary)
        }
        .padding(.vertical, 4)
    }
}

#Preview {
    CommandBrowserView()
}
