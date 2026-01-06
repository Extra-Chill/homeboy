import SwiftUI

struct WPCLITerminalView: View {
    @StateObject private var viewModel = WPCLITerminalViewModel()
    @State private var showingAddCommand = false
    @State private var newCommandName = ""
    @State private var newCommandText = ""
    
    var body: some View {
        VStack(spacing: 0) {
            // Header with path
            HStack {
                Image(systemName: "terminal")
                    .foregroundColor(.accentColor)
                Text("WP-CLI Terminal")
                    .font(.headline)
                Spacer()
                Text(viewModel.localWPPath)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .padding()
            
            Divider()
            
            // Terminal output
            ScrollViewReader { proxy in
                ScrollView {
                    Text(viewModel.output.isEmpty ? "$ " : viewModel.output)
                        .font(.system(.body, design: .monospaced))
                        .foregroundColor(viewModel.output.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding()
                        .id("terminal-bottom")
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: viewModel.output) { _, _ in
                    proxy.scrollTo("terminal-bottom", anchor: .bottom)
                }
            }
            
            Divider()
            
            // Command input
            HStack(spacing: 12) {
                Text("$")
                    .font(.system(.body, design: .monospaced))
                    .foregroundColor(.secondary)
                
                TextField("Enter WP-CLI command...", text: $viewModel.command)
                    .textFieldStyle(.plain)
                    .font(.system(.body, design: .monospaced))
                    .onSubmit {
                        viewModel.runCommand()
                    }
                    .onKeyPress(.upArrow) {
                        viewModel.navigateHistory(direction: -1)
                        return .handled
                    }
                    .onKeyPress(.downArrow) {
                        viewModel.navigateHistory(direction: 1)
                        return .handled
                    }
                    .disabled(viewModel.isRunning)
                
                if viewModel.isRunning {
                    ProgressView()
                        .controlSize(.small)
                    
                    Button("Cancel") {
                        viewModel.cancelCommand()
                    }
                    .buttonStyle(.bordered)
                } else {
                    Button("Run") {
                        viewModel.runCommand()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(viewModel.command.isEmpty)
                    .keyboardShortcut(.return, modifiers: [])
                }
                
                Button {
                    viewModel.clearOutput()
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Clear output")
            }
            .padding()
            
            Divider()
            
            // Saved commands
            HStack(spacing: 8) {
                Text("Saved:")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                ForEach(viewModel.savedCommands) { saved in
                    Button(saved.name) {
                        viewModel.runSavedCommand(saved)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .contextMenu {
                        Button("Remove") {
                            viewModel.removeSavedCommand(saved)
                        }
                    }
                }
                
                Button {
                    showingAddCommand = true
                } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Add saved command")
                
                Spacer()
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
        .frame(minWidth: 600, minHeight: 400)
        .sheet(isPresented: $showingAddCommand) {
            addCommandSheet
        }
    }
    
    private var addCommandSheet: some View {
        VStack(spacing: 16) {
            Text("Add Saved Command")
                .font(.headline)
            
            TextField("Name (e.g., 'Clear Cache')", text: $newCommandName)
                .textFieldStyle(.roundedBorder)
            
            TextField("Command (e.g., 'wp cache flush')", text: $newCommandText)
                .textFieldStyle(.roundedBorder)
            
            HStack {
                Button("Cancel") {
                    showingAddCommand = false
                    newCommandName = ""
                    newCommandText = ""
                }
                .buttonStyle(.bordered)
                
                Button("Add") {
                    viewModel.addSavedCommand(name: newCommandName, command: newCommandText)
                    showingAddCommand = false
                    newCommandName = ""
                    newCommandText = ""
                }
                .buttonStyle(.borderedProminent)
                .disabled(newCommandName.isEmpty || newCommandText.isEmpty)
            }
        }
        .padding(30)
        .frame(width: 400)
    }
}

#Preview {
    WPCLITerminalView()
}
