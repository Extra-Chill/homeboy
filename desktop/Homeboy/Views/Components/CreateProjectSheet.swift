import SwiftUI

/// Modal sheet for creating a new project.
/// Shown when no projects exist or when user explicitly adds a project.
struct CreateProjectSheet: View {
    @ObservedObject var configManager = ConfigurationManager.shared
    @EnvironmentObject var authManager: AuthManager
    
    @Environment(\.dismiss) private var dismiss
    
    @State private var projectName = ""
    @State private var projectId = ""
    @State private var idWasManuallyEdited = false
    @State private var projectType: String = ""
    @FocusState private var nameFieldFocused: Bool
    
    var isFirstProject: Bool = false
    var onProjectCreated: ((ProjectConfiguration) -> Void)?
    
    private var availableProjectTypes: [ProjectTypeDefinition] {
        ProjectTypeManager.shared.allTypes
    }
    
    private var isFormValid: Bool {
        !projectName.isEmpty && !projectId.isEmpty && configManager.isIdAvailable(projectId)
    }
    
    private var idValidationMessage: String? {
        guard !projectId.isEmpty else { return nil }
        if !configManager.isIdAvailable(projectId) {
            return "A project with this ID already exists"
        }
        return nil
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            VStack(spacing: 8) {
                if isFirstProject {
                    Image(systemName: "folder.badge.plus")
                        .font(.system(size: 48))
                        .foregroundColor(.accentColor)
                    Text("Welcome to Homeboy")
                        .font(.title2)
                        .fontWeight(.semibold)
                    Text("Create your first project to get started")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                } else {
                    Text("Add New Project")
                        .font(.headline)
                }
            }
            .padding()
            
            Divider()
            
            // Form
            Form {
                Section {
                    TextField("Project Name", text: $projectName)
                        .textFieldStyle(.roundedBorder)
                        .focused($nameFieldFocused)
                        .onChange(of: nameFieldFocused) { _, isFocused in
                            // Auto-generate ID when name field loses focus
                            if !isFocused && !idWasManuallyEdited && projectId.isEmpty {
                                projectId = configManager.slugFromName(projectName)
                            }
                        }
                    
                    TextField("Project ID", text: $projectId)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: projectId) { oldValue, newValue in
                            // Only mark as manually edited if user actually changed it
                            if !oldValue.isEmpty || !newValue.isEmpty {
                                idWasManuallyEdited = true
                            }
                        }
                    
                    if let message = idValidationMessage {
                        Text(message)
                            .font(.caption)
                            .foregroundColor(.red)
                    } else if !projectId.isEmpty {
                        Text("Used for CLI commands and file storage")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    
                    Picker("Project Type", selection: $projectType) {
                        ForEach(availableProjectTypes) { type in
                            Label(type.displayName, systemImage: type.icon)
                                .tag(type.id)
                        }
                    }
                }
            }
            .formStyle(.grouped)
            .padding()
            
            Divider()
            
            // Actions
            HStack {
                if !isFirstProject {
                    Button("Cancel") {
                        dismiss()
                    }
                    .keyboardShortcut(.cancelAction)
                }
                
                Spacer()
                
                Button(isFirstProject ? "Create Project" : "Add Project") {
                    createProject()
                }
                .buttonStyle(.borderedProminent)
                .disabled(!isFormValid)
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 400, height: isFirstProject ? 420 : 340)
        .onAppear {
            // Set default project type to first available
            if let firstType = availableProjectTypes.first {
                projectType = firstType.id
            }
        }
    }
    
    private func createProject() {
        let project = configManager.createProject(
            id: projectId.trimmingCharacters(in: .whitespacesAndNewlines),
            name: projectName.trimmingCharacters(in: .whitespacesAndNewlines),
            projectType: projectType
        )
        
        // Switch to the new project
        configManager.switchToProject(id: project.id)
        
        onProjectCreated?(project)
        dismiss()
    }
}

#Preview("First Project") {
    CreateProjectSheet(isFirstProject: true)
        .environmentObject(AuthManager())
}

#Preview("Add Project") {
    CreateProjectSheet(isFirstProject: false)
        .environmentObject(AuthManager())
}
