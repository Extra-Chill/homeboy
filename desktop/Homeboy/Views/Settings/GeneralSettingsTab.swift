import SwiftUI

struct GeneralSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    @ObservedObject private var cliInstaller = CLIInstaller.shared
    
    @State private var editedName: String = ""
    @State private var renameError: String?
    @State private var isEditing: Bool = false
    
    var body: some View {
        Form {
            Section("Project Information") {
                LabeledContent("Project ID", value: config.safeActiveProject.id)
                
                HStack {
                    TextField("Project Name", text: $editedName, onCommit: commitRename)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: editedName) { _, newValue in
                            isEditing = newValue != config.safeActiveProject.name
                            // Clear error when user starts typing
                            if renameError != nil { renameError = nil }
                        }
                    
                    if isEditing {
                        Button("Save") {
                            commitRename()
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(editedName.isEmpty)
                    }
                }
                
                if let error = renameError {
                    Text(error)
                        .font(.caption)
                        .foregroundColor(.red)
                }
                
                TextField("Domain", text: Binding(
                    get: { config.safeActiveProject.domain },
                    set: { newValue in
                        config.updateActiveProject { $0.domain = newValue }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                
                Text("Used for display and identification (e.g., example.com)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("Command Line Tool") {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Terminal Command")
                            .font(.headline)
                        Text(cliInstaller.isInstalled ? "Installed at /usr/local/bin/homeboy" : "Not installed")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    Spacer()
                    Button(cliInstaller.isInstalled ? "Uninstall" : "Install") {
                        if cliInstaller.isInstalled {
                            cliInstaller.uninstall()
                        } else {
                            cliInstaller.install()
                        }
                    }
                }
            }
            
            Section("About") {
                LabeledContent("Version", value: ContentContext.appVersion)
                LabeledContent("Build", value: ContentContext.appBuild)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            editedName = config.safeActiveProject.name
        }
        .onChange(of: config.activeProject?.id) { _, _ in
            editedName = config.safeActiveProject.name
            isEditing = false
            renameError = nil
        }
    }
    
    private func commitRename() {
        guard let project = config.activeProject else { return }
        guard !editedName.isEmpty else { return }
        
        let result = config.renameProject(project, to: editedName)
        switch result {
        case .success:
            isEditing = false
            renameError = nil
        case .failure(let error):
            renameError = error.localizedDescription
        }
    }
}
