import SwiftUI

struct ModulesSettingsTab: View {
    @ObservedObject private var moduleManager = ModuleManager.shared
    @State private var showInstallSheet = false
    @State private var selectedModuleForUninstall: LoadedModule?
    @State private var showUninstallConfirmation = false
    
    
    var body: some View {
        Form {
            Section("Installed Modules") {
                if moduleManager.modules.isEmpty {
                    ContentUnavailableView(
                        "No Modules Installed",
                        systemImage: "puzzlepiece.extension",
                        description: Text("Install modules to extend functionality")
                    )
                } else {
                    ForEach(moduleManager.modules) { module in
                        moduleRow(module)
                    }
                }
            }
            
            Section {
                Button {
                    showInstallSheet = true
                } label: {
                    Label("Install Module from Folder...", systemImage: "plus.circle")
                }
                
                Button {
                    moduleManager.loadModules()
                } label: {
                    Label("Refresh Modules", systemImage: "arrow.clockwise")
                }
            }
            
            Section("Module Directory") {
                HStack {
                    Text(moduleManager.modulesDirectory.path)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.secondary)
                    
                    Spacer()
                    
                    Button {
                        NSWorkspace.shared.open(moduleManager.modulesDirectory)
                    } label: {
                        Image(systemName: "folder")
                    }
                    .buttonStyle(.plain)
                    .help("Open in Finder")
                }
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showInstallSheet) {
            ModuleInstallSheet()
        }
        .confirmationDialog(
            "Uninstall Module",
            isPresented: $showUninstallConfirmation,
            titleVisibility: .visible
        ) {
            Button("Uninstall", role: .destructive) {
                if let module = selectedModuleForUninstall {
                    _ = moduleManager.uninstallModule(moduleId: module.id)
                }
                selectedModuleForUninstall = nil
            }
            Button("Cancel", role: .cancel) {
                selectedModuleForUninstall = nil
            }
        } message: {
            if let module = selectedModuleForUninstall {
                Text("Are you sure you want to uninstall \"\(module.name)\"? This will delete all module files including its virtual environment.")
            }
        }
    }
    
    // MARK: - Module Row
    
    @ViewBuilder
    private func moduleRow(_ module: LoadedModule) -> some View {
        HStack {
            Image(systemName: module.icon)
                .font(.title2)
                .foregroundColor(.accentColor)
                .frame(width: 32)
            
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(module.name)
                        .font(.headline)
                    
                    Text("v\(module.manifest.version)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Text(module.manifest.description)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                
                Text("by \(module.manifest.author)")
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            // Status badge
                        statusBadge(for: module.state, moduleName: module.name)
            
            // Remove button
            Button("Remove") {
                selectedModuleForUninstall = module
                showUninstallConfirmation = true
            }
            .buttonStyle(.bordered)
            .tint(.red)
            .controlSize(.small)
        }
        .padding(.vertical, 4)
    }
    
    @ViewBuilder
    private func statusBadge(for state: ModuleState, moduleName: String) -> some View {
        switch state {
        case .ready:
            Label("Ready", systemImage: "checkmark.circle.fill")
                .font(.caption)
                .foregroundColor(.green)
        case .needsSetup:
            Label("Setup Required", systemImage: "exclamationmark.circle.fill")
                .font(.caption)
                .foregroundColor(.orange)
                .contextMenu {
                    Button("Copy Warning") {
                        AppWarning(
                            "Setup Required",
                            source: "Module: \(moduleName)"
                        ).copyToClipboard()
                    }
                }
        case .installing:
            Label("Installing...", systemImage: "arrow.down.circle")
                .font(.caption)
                .foregroundColor(.blue)
        case .missingRequirements(let components):
            Label("Missing: \(components.joined(separator: ", "))", systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundColor(.gray)
                .help("Requires: \(components.joined(separator: ", "))")
                .contextMenu {
                    Button("Copy Warning") {
                        AppWarning(
                            "Missing requirements: \(components.joined(separator: ", "))",
                            source: "Module: \(moduleName)"
                        ).copyToClipboard()
                    }
                }
        case .error(let message):
            Label("Error", systemImage: "xmark.circle.fill")
                .font(.caption)
                .foregroundColor(.red)
                .help(message)
                .contextMenu {
                    Button("Copy Error") {
                        AppError(
                            message,
                            source: "Module: \(moduleName)"
                        ).copyToClipboard()
                    }
                }
        }
    }

}

