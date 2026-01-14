import SwiftUI

/// Modal sheet for creating a new project.
/// Shown when no projects exist or when user explicitly adds a project.
struct CreateProjectSheet: View {
    @ObservedObject var configManager = ConfigurationManager.shared
    @EnvironmentObject var authManager: AuthManager

    @Environment(\.dismiss) private var dismiss

    @State private var projectName = ""
    @State private var projectDomain = ""
    @State private var isCreating = false
    @State private var errorMessage: String?

    var isFirstProject: Bool = false
    var onProjectCreated: ((ProjectConfiguration) -> Void)?

    private var isFormValid: Bool {
        !projectName.isEmpty && !projectDomain.isEmpty && !isCreating
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

                    TextField("Domain", text: $projectDomain)
                        .textFieldStyle(.roundedBorder)

                    Text("e.g., example.com")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                if let error = errorMessage {
                    Text(error)
                        .font(.caption)
                        .foregroundColor(.red)
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

                if isCreating {
                    ProgressView()
                        .controlSize(.small)
                        .padding(.trailing, 8)
                }

                Button(isFirstProject ? "Create Project" : "Add Project") {
                    createProject()
                }
                .buttonStyle(.borderedProminent)
                .disabled(!isFormValid)
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 400, height: isFirstProject ? 380 : 300)
    }

    private func createProject() {
        isCreating = true
        errorMessage = nil

        Task {
            do {
                let project = try await configManager.createProject(
                    name: projectName.trimmingCharacters(in: .whitespacesAndNewlines),
                    domain: projectDomain.trimmingCharacters(in: .whitespacesAndNewlines)
                )

                await configManager.switchToProject(id: project.id)
                onProjectCreated?(project)
                dismiss()
            } catch {
                errorMessage = error.localizedDescription
                isCreating = false
            }
        }
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
