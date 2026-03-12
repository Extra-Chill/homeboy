import SwiftUI

/// Main container view for rendering a extension based on its manifest
struct ExtensionContainerView: View {
    let extensionId: String
    
    @StateObject private var viewModel: ExtensionViewModel
    @ObservedObject private var extensionManager = ExtensionManager.shared
    
    init(extensionId: String) {
        self.extensionId = extensionId
        _viewModel = StateObject(wrappedValue: ExtensionViewModel(extensionId: extensionId))
    }
    
    private var extension: LoadedExtension? {
        extensionManager.extension(withId: extensionId)
    }
    
    var body: some View {
        Group {
            if let extension = extension {
                extensionContent(extension)
                    .onAppear {
                        viewModel.initializeInputValues(from: extension)
                    }
            } else {
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Extension Not Found",
                        systemImage: "exclamationmark.triangle",
                        description: Text("The extension '\(extensionId)' could not be loaded.")
                    )
                    CopyButton.error(
                        "Extension '\(extensionId)' could not be loaded",
                        source: "Extension Container"
                    )
                }
            }
        }
    }
    
    @ViewBuilder
    private func extensionContent(_ extension: LoadedExtension) -> some View {
        VStack(spacing: 0) {
            // Header
            ExtensionHeaderView(extension: extension, viewModel: viewModel)
            
            Divider()
            
            // Main content based on extension state
            switch extension.state {
            case .needsSetup:
                ExtensionSetupView(extension: extension, viewModel: viewModel)
                
            case .installing:
                VStack {
                    ProgressView("Installing dependencies...")
                    ExtensionConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                }
                .padding()
                
            case .missingRequirements(let requirements):
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Missing Requirements",
                        systemImage: "exclamationmark.triangle",
                        description: Text("This extension requires:\n\(requirements.joined(separator: ", "))")
                    )
                    CopyButton.warning(
                        "Missing requirements: \(requirements.joined(separator: ", "))",
                        source: "Extension: \(extension.name)"
                    )
                }
                
            case .error(let message):
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Extension Error",
                        systemImage: "exclamationmark.triangle",
                        description: Text(message)
                    )
                    CopyButton.error(message, source: "Extension: \(extension.name)")
                }
                
            case .ready:
                if let _ = extension.manifest.runtime {
                    ExtensionReadyView(extension: extension, viewModel: viewModel)
                } else {
                    PlatformExtensionView(extension: extension)
                }
            }
        }
    }
}

// MARK: - Platform Extension View (extensions without runtime, e.g. OpenClaw)

struct PlatformExtensionView: View {
    let extension: LoadedExtension
    @State private var actionOutput: String = ""
    @State private var isRunning = false
    
    var body: some View {
        VStack(spacing: 0) {
            // Actions grid
            if let actions = extension.manifest.actions, !actions.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Actions")
                        .font(.headline)
                    
                    LazyVGrid(columns: [
                        GridItem(.adaptive(minimum: 160))
                    ], spacing: 8) {
                        ForEach(actions) { action in
                            Button {
                                Task { await runAction(action) }
                            } label: {
                                HStack {
                                    Text(action.label)
                                    Spacer()
                                    if isRunning {
                                        ProgressView()
                                            .controlSize(.small)
                                    }
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                                .background(Color.accentColor.opacity(0.1))
                                .cornerRadius(8)
                            }
                            .buttonStyle(.plain)
                            .disabled(isRunning)
                        }
                    }
                }
                .padding()
            }
            
            Divider()
            
            // Output console
            if !actionOutput.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("Output")
                            .font(.headline)
                        Spacer()
                        Button("Clear") {
                            actionOutput = ""
                        }
                        .buttonStyle(.plain)
                        .foregroundColor(.secondary)
                    }
                    
                    ScrollView {
                        Text(actionOutput)
                            .font(.system(.body, design: .monospaced))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                }
                .padding()
            } else {
                Spacer()
            }
        }
    }
    
    private func runAction(_ action: ActionConfig) async {
        guard let command = action.command else { return }
        isRunning = true
        defer { isRunning = false }
        
        do {
            let args = ["extension", "action", extension.id, action.id]
            let response = try await CLIBridge.shared.execute(args)
            actionOutput += "$ \(command)\n\(response.output)\n\n"
        } catch {
            actionOutput += "Error: \(error.localizedDescription)\n\n"
        }
    }
}

// MARK: - Header View

struct ExtensionHeaderView: View {
    let extension: LoadedExtension
    @ObservedObject var viewModel: ExtensionViewModel
    
    var body: some View {
        HStack {
            Image(systemName: extension.icon)
                .font(.title2)
                .foregroundColor(.accentColor)
            
            VStack(alignment: .leading, spacing: 2) {
                Text(extension.name)
                    .font(.headline)
                Text(extension.manifest.description)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            if viewModel.isRunning {
                Button("Cancel") {
                    viewModel.cancel()
                }
                .buttonStyle(.bordered)
            }
        }
        .padding()
    }
}

// MARK: - Ready State View

struct ExtensionReadyView: View {
    let extension: LoadedExtension
    @ObservedObject var viewModel: ExtensionViewModel
    
    @State private var showConsole = true
    
    var body: some View {
        HSplitView {
            // Left side: Inputs + Console
            VStack(spacing: 0) {
                // Input form
                ExtensionInputsView(extension: extension, viewModel: viewModel)
                
                Divider()
                
                // Console section
                VStack(spacing: 0) {
                    HStack {
                        Text("Console")
                            .font(.headline)
                        Spacer()
                        Button {
                            showConsole.toggle()
                        } label: {
                            Image(systemName: showConsole ? "chevron.down" : "chevron.right")
                        }
                        .buttonStyle(.plain)
                    }
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    
                    if showConsole {
                        ExtensionConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                    }
                }
            }
            .frame(minWidth: 300)
            
            // Right side: Results (if table display)
            if extension.manifest.output?.display == .table && !viewModel.results.isEmpty {
                VStack(spacing: 0) {
                    ExtensionResultsView(extension: extension, viewModel: viewModel)
                    
                    Divider()
                    
                    ExtensionActionsBar(extension: extension, viewModel: viewModel)
                }
                .frame(minWidth: 400)
            }
        }
    }
}
