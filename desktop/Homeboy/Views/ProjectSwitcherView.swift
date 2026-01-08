import SwiftUI

/// Project switcher dropdown for the sidebar header with project management capabilities
struct ProjectSwitcherView: View {
    @ObservedObject var configManager = ConfigurationManager.shared
    @EnvironmentObject var authManager: AuthManager
    
    /// Optional closure to check if there's unsaved work before switching projects.
    /// Returns true if there's unsaved work that should prompt a confirmation.
    var hasUnsavedWork: (() -> Bool)?
    
    @State private var showManageProjects = false
    @State private var showAddProject = false
    @State private var showDeleteConfirmation = false
    @State private var projectToDelete: String?
    @State private var showUnsavedWorkAlert = false
    @State private var pendingProjectSwitch: String?
    
    // Add project form
    @State private var newProjectName = ""
    @State private var newProjectId = ""
    @State private var newProjectIdWasManuallyEdited = false
    @State private var newProjectType: String = "wordpress"
    
    private var availableProjects: [ProjectConfiguration] {
        configManager.availableProjectIds().compactMap { configManager.loadProject(id: $0) }
    }
    
    private var availableProjectTypes: [ProjectTypeDefinition] {
        ProjectTypeManager.shared.allTypes
    }
    
    private var isFormValid: Bool {
        !newProjectName.isEmpty && !newProjectId.isEmpty && configManager.isIdAvailable(newProjectId)
    }
    
    private var idValidationMessage: String? {
        guard !newProjectId.isEmpty else { return nil }
        if !configManager.isIdAvailable(newProjectId) {
            return "A project with this ID already exists"
        }
        return nil
    }
    
    var body: some View {
        Menu {
            // Project list
            ForEach(availableProjects, id: \.id) { project in
                Button {
                    switchToProject(project.id)
                } label: {
                    HStack {
                        if project.id == configManager.activeProject?.id {
                            Image(systemName: "checkmark")
                        }
                        Text(project.name)
                    }
                }
            }
            
            Divider()
            
            // Manage projects
            Button {
                showManageProjects = true
            } label: {
                Label("Manage Projects...", systemImage: "gear")
            }
        } label: {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(configManager.activeProject?.name ?? "No Project")
                        .font(.headline)
                        .lineLimit(1)
                    if let domain = configManager.activeProject?.domain, !domain.isEmpty {
                        Text(domain)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer()
                Image(systemName: "chevron.up.chevron.down")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Color(NSColor.controlBackgroundColor))
            .cornerRadius(6)
        }
        .buttonStyle(.plain)
        .sheet(isPresented: $showManageProjects) {
            manageProjectsSheet
        }
        .sheet(isPresented: $showAddProject) {
            addProjectSheet
        }
        .alert("Delete Project", isPresented: $showDeleteConfirmation) {
            Button("Cancel", role: .cancel) {
                projectToDelete = nil
            }
            Button("Delete", role: .destructive) {
                if let id = projectToDelete {
                    configManager.deleteProject(id: id)
                    projectToDelete = nil
                }
            }
        } message: {
            if let id = projectToDelete, let project = configManager.loadProject(id: id) {
                Text("Are you sure you want to delete \"\(project.name)\"? This cannot be undone.")
            }
        }
        .alert("Unsaved Changes", isPresented: $showUnsavedWorkAlert) {
            Button("Cancel", role: .cancel) {
                pendingProjectSwitch = nil
            }
            Button("Discard Changes", role: .destructive) {
                if let projectId = pendingProjectSwitch {
                    performProjectSwitch(projectId)
                    pendingProjectSwitch = nil
                }
            }
        } message: {
            Text("You have unsaved changes. Switching projects will discard them.")
        }
    }
    
    // MARK: - Manage Projects Sheet
    
    private var manageProjectsSheet: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Manage Projects")
                    .font(.headline)
                Spacer()
                Button("Done") {
                    showManageProjects = false
                }
                .buttonStyle(.borderedProminent)
            }
            .padding()
            
            Divider()
            
            // Project list
            List {
                ForEach(availableProjects, id: \.id) { project in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(project.name)
                                .fontWeight(project.id == configManager.activeProject?.id ? .semibold : .regular)
                            if !project.domain.isEmpty {
                                Text(project.domain)
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                        }
                        
                        Spacer()
                        
                        if project.id == configManager.activeProject?.id {
                            Text("Active")
                                .font(.caption)
                                .foregroundColor(.secondary)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.2))
                                .cornerRadius(4)
                        } else {
                            Button(role: .destructive) {
                                projectToDelete = project.id
                                showDeleteConfirmation = true
                            } label: {
                                Image(systemName: "trash")
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                    .padding(.vertical, 4)
                }
            }
            
            Divider()
            
            // Add project button
            HStack {
                Button {
                    showManageProjects = false
                    showAddProject = true
                } label: {
                    Label("Add Project", systemImage: "plus")
                }
                Spacer()
            }
            .padding()
        }
        .frame(width: 400, height: 350)
    }
    
    // MARK: - Add Project Sheet
    
    private var addProjectSheet: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Add New Project")
                    .font(.headline)
                Spacer()
            }
            .padding()
            
            Divider()
            
            // Form
            Form {
                Section("Project Information") {
                    TextField("Project Name", text: $newProjectName)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: newProjectName) { _, newValue in
                            if !newProjectIdWasManuallyEdited {
                                newProjectId = configManager.slugFromName(newValue)
                            }
                        }
                    
                    TextField("Project ID", text: $newProjectId)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: newProjectId) { _, _ in
                            newProjectIdWasManuallyEdited = true
                        }
                    
                    if let message = idValidationMessage {
                        Text(message)
                            .font(.caption)
                            .foregroundColor(.red)
                    } else if !newProjectId.isEmpty {
                        Text("Used for CLI commands and file storage")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    
                    Picker("Project Type", selection: $newProjectType) {
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
                Button("Cancel") {
                    resetAddProjectForm()
                    showAddProject = false
                }
                .keyboardShortcut(.cancelAction)
                
                Spacer()
                
                Button("Add Project") {
                    addProject()
                }
                .buttonStyle(.borderedProminent)
                .disabled(!isFormValid)
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 400, height: 360)
    }
    
    // MARK: - Actions
    
    private func switchToProject(_ projectId: String) {
        guard projectId != configManager.activeProject?.id else { return }
        
        // Check for unsaved work
        if let checkUnsaved = hasUnsavedWork, checkUnsaved() {
            pendingProjectSwitch = projectId
            showUnsavedWorkAlert = true
            return
        }
        
        performProjectSwitch(projectId)
    }
    
    private func performProjectSwitch(_ projectId: String) {
        Task {
            configManager.switchToProject(id: projectId)
            await authManager.resetForProjectSwitch()
        }
    }
    
    private func addProject() {
        let project = configManager.createProject(
            id: newProjectId.trimmingCharacters(in: .whitespacesAndNewlines),
            name: newProjectName.trimmingCharacters(in: .whitespacesAndNewlines),
            projectType: newProjectType
        )
        
        resetAddProjectForm()
        showAddProject = false
        
        // Switch to the new project so user can configure it
        switchToProject(project.id)
    }
    
    private func resetAddProjectForm() {
        newProjectName = ""
        newProjectId = ""
        newProjectIdWasManuallyEdited = false
        newProjectType = availableProjectTypes.first?.id ?? "wordpress"
    }
}

#Preview {
    ProjectSwitcherView()
        .environmentObject(AuthManager())
        .frame(width: 220)
        .padding()
}
