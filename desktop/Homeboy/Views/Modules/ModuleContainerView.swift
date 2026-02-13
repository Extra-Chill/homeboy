import SwiftUI

/// Main container view for rendering a module based on its manifest
struct ModuleContainerView: View {
    let moduleId: String
    
    @StateObject private var viewModel: ModuleViewModel
    @ObservedObject private var moduleManager = ModuleManager.shared
    
    init(moduleId: String) {
        self.moduleId = moduleId
        _viewModel = StateObject(wrappedValue: ModuleViewModel(moduleId: moduleId))
    }
    
    private var module: LoadedModule? {
        moduleManager.module(withId: moduleId)
    }
    
    var body: some View {
        Group {
            if let module = module {
                moduleContent(module)
                    .onAppear {
                        viewModel.initializeInputValues(from: module)
                    }
            } else {
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Module Not Found",
                        systemImage: "exclamationmark.triangle",
                        description: Text("The module '\(moduleId)' could not be loaded.")
                    )
                    CopyButton.error(
                        "Module '\(moduleId)' could not be loaded",
                        source: "Module Container"
                    )
                }
            }
        }
    }
    
    @ViewBuilder
    private func moduleContent(_ module: LoadedModule) -> some View {
        VStack(spacing: 0) {
            // Header
            ModuleHeaderView(module: module, viewModel: viewModel)
            
            Divider()
            
            // Main content based on module state
            switch module.state {
            case .needsSetup:
                ModuleSetupView(module: module, viewModel: viewModel)
                
            case .installing:
                VStack {
                    ProgressView("Installing dependencies...")
                    ModuleConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                }
                .padding()
                
            case .missingRequirements(let requirements):
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Missing Requirements",
                        systemImage: "exclamationmark.triangle",
                        description: Text("This module requires:\n\(requirements.joined(separator: ", "))")
                    )
                    CopyButton.warning(
                        "Missing requirements: \(requirements.joined(separator: ", "))",
                        source: "Module: \(module.name)"
                    )
                }
                
            case .error(let message):
                VStack(spacing: 16) {
                    ContentUnavailableView(
                        "Module Error",
                        systemImage: "exclamationmark.triangle",
                        description: Text(message)
                    )
                    CopyButton.error(message, source: "Module: \(module.name)")
                }
                
            case .ready:
                if let _ = module.manifest.runtime {
                    ModuleReadyView(module: module, viewModel: viewModel)
                } else {
                    PlatformModuleView(module: module)
                }
            }
        }
    }
}

// MARK: - Platform Module View (modules without runtime, e.g. OpenClaw)

struct PlatformModuleView: View {
    let module: LoadedModule
    @State private var actionOutput: String = ""
    @State private var isRunning = false
    
    var body: some View {
        VStack(spacing: 0) {
            // Actions grid
            if let actions = module.manifest.actions, !actions.isEmpty {
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
            let args = ["module", "action", module.id, action.id]
            let response = try await CLIBridge.shared.execute(args)
            actionOutput += "$ \(command)\n\(response.output)\n\n"
        } catch {
            actionOutput += "Error: \(error.localizedDescription)\n\n"
        }
    }
}

// MARK: - Header View

struct ModuleHeaderView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    var body: some View {
        HStack {
            Image(systemName: module.icon)
                .font(.title2)
                .foregroundColor(.accentColor)
            
            VStack(alignment: .leading, spacing: 2) {
                Text(module.name)
                    .font(.headline)
                Text(module.manifest.description)
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

struct ModuleReadyView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    @State private var showConsole = true
    
    var body: some View {
        HSplitView {
            // Left side: Inputs + Console
            VStack(spacing: 0) {
                // Input form
                ModuleInputsView(module: module, viewModel: viewModel)
                
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
                        ModuleConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                    }
                }
            }
            .frame(minWidth: 300)
            
            // Right side: Results (if table display)
            if module.manifest.output?.display == .table && !viewModel.results.isEmpty {
                VStack(spacing: 0) {
                    ModuleResultsView(module: module, viewModel: viewModel)
                    
                    Divider()
                    
                    ModuleActionsBar(module: module, viewModel: viewModel)
                }
                .frame(minWidth: 400)
            }
        }
    }
}
