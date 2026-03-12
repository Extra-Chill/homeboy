import SwiftUI

struct ExtensionsSettingsTab: View {
    @ObservedObject private var extensionManager = ExtensionManager.shared
    @State private var showInstallSheet = false
    @State private var selectedExtensionForUninstall: LoadedExtension?
    @State private var showUninstallConfirmation = false
    
    
    var body: some View {
        Form {
            Section("Installed Extensions") {
                if extensionManager.extensions.isEmpty {
                    ContentUnavailableView(
                        "No Extensions Installed",
                        systemImage: "puzzlepiece.extension",
                        description: Text("Install extensions to extend functionality")
                    )
                } else {
                    ForEach(extensionManager.extensions) { extension in
                        extensionRow(extension)
                    }
                }
            }
            
            Section {
                Button {
                    showInstallSheet = true
                } label: {
                    Label("Install Extension from Folder...", systemImage: "plus.circle")
                }
                
                Button {
                    Task { await extensionManager.loadExtensions() }
                } label: {
                    Label("Refresh Extensions", systemImage: "arrow.clockwise")
                }
            }
            
            Section("Extension Directory") {
                HStack {
                    Text(AppPaths.extensions.path)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.secondary)

                    Spacer()

                    Button {
                        NSWorkspace.shared.open(AppPaths.extensions)
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
            ExtensionInstallSheet()
        }
        .confirmationDialog(
            "Uninstall Extension",
            isPresented: $showUninstallConfirmation,
            titleVisibility: .visible
        ) {
            Button("Uninstall", role: .destructive) {
                if let extension = selectedExtensionForUninstall {
                    Task { try? await extensionManager.uninstallExtension(extensionId: extension.id) }
                }
                selectedExtensionForUninstall = nil
            }
            Button("Cancel", role: .cancel) {
                selectedExtensionForUninstall = nil
            }
        } message: {
            if let extension = selectedExtensionForUninstall {
                Text("Are you sure you want to uninstall \"\(extension.name)\"? This will delete all extension files including its virtual environment.")
            }
        }
    }
    
    // MARK: - Extension Row
    
    @ViewBuilder
    private func extensionRow(_ extension: LoadedExtension) -> some View {
        HStack {
            Image(systemName: extension.icon)
                .font(.title2)
                .foregroundColor(.accentColor)
                .frame(width: 32)
            
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(extension.name)
                        .font(.headline)
                    
                    Text("v\(extension.manifest.version)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                
                Text(extension.manifest.description)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                
                Text("by \(extension.manifest.author)")
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            // Status badge
                        statusBadge(for: extension.state, extensionName: extension.name)
            
            // Remove button
            Button("Remove") {
                selectedExtensionForUninstall = extension
                showUninstallConfirmation = true
            }
            .buttonStyle(.bordered)
            .tint(.red)
            .controlSize(.small)
        }
        .padding(.vertical, 4)
    }
    
    @ViewBuilder
    private func statusBadge(for state: ExtensionState, extensionName: String) -> some View {
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
                            source: "Extension: \(extensionName)"
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
                            source: "Extension: \(extensionName)"
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
                            source: "Extension: \(extensionName)"
                        ).copyToClipboard()
                    }
                }
        }
    }

}

