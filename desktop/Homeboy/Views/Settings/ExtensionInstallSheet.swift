import SwiftUI

struct ExtensionInstallSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var extensionManager = ExtensionManager.shared
    
    @State private var selectedPath: URL?
    @State private var isInstalling = false
    @State private var error: (any DisplayableError)?
    @State private var success = false
    
    var body: some View {
        VStack(spacing: 20) {
            // Header
            HStack {
                Text("Install Extension")
                    .font(.headline)
                Spacer()
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
            
            Divider()
            
            // Instructions
            VStack(alignment: .leading, spacing: 8) {
                Text("Select a folder containing a extension.json manifest file.")
                    .foregroundColor(.secondary)
                
                Text("The extension will be copied to the application's extensions directory.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            
            // Folder selection
            HStack {
                if let path = selectedPath {
                    Text(path.lastPathComponent)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    
                    Button {
                        selectedPath = nil
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                } else {
                    Text("No folder selected")
                        .foregroundColor(.secondary)
                }
                
                Spacer()
                
                Button("Choose Folder...") {
                    chooseFolder()
                }
            }
            .padding()
            .background(Color(nsColor: .controlBackgroundColor))
            .cornerRadius(8)
            
            // Error or success message
            if let error = error {
                InlineErrorView(error)
            }
            
            if success {
                Label("Extension installed successfully!", systemImage: "checkmark.circle.fill")
                    .foregroundColor(.green)
            }
            
            Spacer()
            
            // Actions
            HStack {
                Spacer()
                
                Button("Install") {
                    installExtension()
                }
                .buttonStyle(.borderedProminent)
                .disabled(selectedPath == nil || isInstalling)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 450, height: 300)
    }
    
    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Select a extension folder containing extension.json"
        
        if panel.runModal() == .OK {
            selectedPath = panel.url
            error = nil
            success = false
            
                // Validate that extension.json exists
                if let path = panel.url {
                    let manifestPath = path.appendingPathComponent("extension.json")
                    if !FileManager.default.fileExists(atPath: manifestPath.path) {
                        error = AppError("Selected folder does not contain a extension.json file", source: "Extension Installer")
                        selectedPath = nil
                    }
                }
        }
    }
    
    private func installExtension() {
        guard let path = selectedPath else { return }

        isInstalling = true
        error = nil
        success = false

        Task {
            do {
                try await extensionManager.installExtension(from: path.path)
                await MainActor.run {
                    success = true
                    isInstalling = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                        dismiss()
                    }
                }
            } catch {
                await MainActor.run {
                    self.error = error.toDisplayableError(source: "Extension Installer")
                    isInstalling = false
                }
            }
        }
    }
}
