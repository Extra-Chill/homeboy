import SwiftUI

struct ModuleInstallSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var moduleManager = ModuleManager.shared
    
    @State private var selectedPath: URL?
    @State private var isInstalling = false
    @State private var error: String?
    @State private var success = false
    
    var body: some View {
        VStack(spacing: 20) {
            // Header
            HStack {
                Text("Install Module")
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
                Text("Select a folder containing a module.json manifest file.")
                    .foregroundColor(.secondary)
                
                Text("The module will be copied to the application's modules directory.")
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
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
            }
            
            if success {
                Label("Module installed successfully!", systemImage: "checkmark.circle.fill")
                    .foregroundColor(.green)
            }
            
            Spacer()
            
            // Actions
            HStack {
                Spacer()
                
                Button("Install") {
                    installModule()
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
        panel.message = "Select a module folder containing module.json"
        
        if panel.runModal() == .OK {
            selectedPath = panel.url
            error = nil
            success = false
            
            // Validate that module.json exists
            if let path = panel.url {
                let manifestPath = path.appendingPathComponent("module.json")
                if !FileManager.default.fileExists(atPath: manifestPath.path) {
                    error = "Selected folder does not contain a module.json file"
                    selectedPath = nil
                }
            }
        }
    }
    
    private func installModule() {
        guard let path = selectedPath else { return }
        
        isInstalling = true
        error = nil
        success = false
        
        let result = moduleManager.installModule(from: path)
        
        switch result {
        case .success:
            success = true
            // Auto-dismiss after brief delay
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                dismiss()
            }
        case .failure(let err):
            error = err.localizedDescription
        }
        
        isInstalling = false
    }
}
