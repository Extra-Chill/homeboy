import SwiftUI
import AppKit

struct ComponentsSettingsTab: View {
    @ObservedObject var config: ConfigurationManager

    @State private var showingAddSheet = false
    @State private var editingComponent: ComponentConfiguration? = nil
    @State private var componentToDelete: ComponentConfiguration? = nil
    @State private var showDeleteConfirmation = false

    private var project: ProjectConfiguration {
        config.safeActiveProject
    }

    private var projectComponents: [ComponentConfiguration] {
        config.loadComponentsForProject(project)
    }

    var body: some View {
        Form {
            Section {
                HStack {
                    Text("Deployable Components")
                        .font(.headline)
                    Spacer()
                    Button("Add Component") {
                        showingAddSheet = true
                    }
                }

                if projectComponents.isEmpty {
                    Text("No components configured. Add components to deploy.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }

            Section("Components") {
                ForEach(projectComponents.sorted { $0.name < $1.name }) { component in
                    ComponentRow(
                        component: component,
                        onEdit: { editingComponent = component },
                        onDelete: {
                            componentToDelete = component
                            showDeleteConfirmation = true
                        }
                    )
                }
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showingAddSheet) {
            AddEditComponentSheet(config: config, existing: nil)
        }
        .sheet(item: $editingComponent) { component in
            AddEditComponentSheet(config: config, existing: component)
        }
        .alert("Delete Component?", isPresented: $showDeleteConfirmation) {
            Button("Cancel", role: .cancel) {
                componentToDelete = nil
            }
            Button("Delete", role: .destructive) {
                if let component = componentToDelete {
                    deleteComponent(component)
                }
                componentToDelete = nil
            }
        } message: {
            Text("Remove \(componentToDelete?.name ?? "") from deployment list? This does not delete the actual files.")
        }
    }

    private func deleteComponent(_ component: ComponentConfiguration) {
        Task {
            try? await config.updateActiveProject { project in
                project.componentIds.removeAll { $0 == component.id }
            }
        }
    }
}

// MARK: - Component Row

struct ComponentRow: View {
    let component: ComponentConfiguration
    let onEdit: () -> Void
    let onDelete: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(component.name)
                    .font(.body)
                Text(truncatedPath(component.localPath))
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }

            Spacer()

            Button("Edit") {
                onEdit()
            }
            .buttonStyle(.borderless)

            Button(role: .destructive) {
                onDelete()
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
        }
        .padding(.vertical, 2)
    }

    private func truncatedPath(_ path: String) -> String {
        let components = path.split(separator: "/")
        if components.count > 4 {
            return ".../" + components.suffix(3).joined(separator: "/")
        }
        return path
    }
}

// MARK: - Add/Edit Component Sheet

struct AddEditComponentSheet: View {
    @ObservedObject var config: ConfigurationManager
    let existing: ComponentConfiguration?

    // Basic info
    @State private var localPath: String = ""
    @State private var id: String = ""
    @State private var name: String = ""

    // Deployment paths
    @State private var remotePath: String = ""
    @State private var buildArtifact: String = ""

    // Version detection
    @State private var versionFile: String = ""
    @State private var versionPattern: String = "Version:\\s*([\\d.]+)"

    @State private var isNetwork: Bool = false
    @State private var validationError: String? = nil

    @Environment(\.dismiss) private var dismiss

    private var isEditing: Bool {
        existing != nil
    }

    private var canSave: Bool {
        !localPath.isEmpty && !id.isEmpty && !name.isEmpty && !remotePath.isEmpty && !buildArtifact.isEmpty
    }

    var body: some View {
        VStack(spacing: 16) {
            Text(isEditing ? "Edit Component" : "Add Component")
                .font(.headline)

            Form {
                Section("Location") {
                    HStack {
                        TextField("Local Path", text: $localPath)
                            .textFieldStyle(.roundedBorder)
                        Button("Browse...") {
                            browseForFolder()
                        }
                    }
                }

                if !localPath.isEmpty {
                    Section("Component Details") {
                        TextField("ID (slug)", text: $id)
                            .textFieldStyle(.roundedBorder)
                        TextField("Display Name", text: $name)
                            .textFieldStyle(.roundedBorder)

                        if config.safeActiveProject.isWordPress {
                            Toggle("Network Plugin", isOn: $isNetwork)
                        }
                    }

                    Section("Deployment") {
                        TextField("Remote Path", text: $remotePath)
                            .textFieldStyle(.roundedBorder)
                        TextField("Build Artifact (e.g., build/my-app.zip)", text: $buildArtifact)
                            .textFieldStyle(.roundedBorder)
                    }

                    Section("Version Detection (Optional)") {
                        TextField("Version File", text: $versionFile)
                            .textFieldStyle(.roundedBorder)
                        TextField("Version Pattern (regex)", text: $versionPattern)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(.body, design: .monospaced))
                    }
                }

                if let error = validationError {
                    Section {
                        InlineErrorView(error, source: "Component Settings")
                    }
                }
            }
            .formStyle(.grouped)

            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Spacer()

                Button(isEditing ? "Save" : "Add Component") {
                    if validate() {
                        save()
                        dismiss()
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave)
            }
        }
        .padding()
        .frame(width: 550, height: 500)
        .onAppear {
            if let existing = existing {
                localPath = existing.localPath
                id = existing.id
                name = existing.name
                remotePath = existing.remotePath
                buildArtifact = existing.buildArtifact
                versionFile = existing.versionFile ?? ""
                versionPattern = existing.versionPattern ?? "Version:\\s*([\\d.]+)"
                isNetwork = existing.isNetwork ?? false
            }
        }
        .onChange(of: localPath) { _, newValue in
            if !isEditing {
                autoDetect()
            }
        }
    }

    private func browseForFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Select a component folder"

        if panel.runModal() == .OK, let url = panel.url {
            localPath = url.path
        }
    }

    private func autoDetect() {
        guard !localPath.isEmpty else { return }

        let url = URL(fileURLWithPath: localPath)
        let slug = url.lastPathComponent
        id = slug

        let styleCSS = "\(localPath)/style.css"
        let mainPHP = "\(localPath)/\(slug).php"

        // Detect type and set defaults
        if FileManager.default.fileExists(atPath: styleCSS) {
            // Theme
            name = VersionParser.parseVersion(from: (try? String(contentsOfFile: styleCSS, encoding: .utf8)) ?? "") != nil
                ? (parseThemeName(from: styleCSS) ?? slug.capitalized)
                : slug.capitalized
            remotePath = "themes/\(slug)"
            buildArtifact = "build/\(slug).zip"
            versionFile = "style.css"
        } else if FileManager.default.fileExists(atPath: mainPHP) {
            // Plugin
            name = parsePluginName(from: mainPHP) ?? slug.capitalized
            remotePath = "plugins/\(slug)"
            buildArtifact = "build/\(slug).zip"
            versionFile = "\(slug).php"
        } else {
            // Generic
            name = slug.capitalized
            remotePath = slug
            buildArtifact = "build/\(slug).zip"
            versionFile = ""
        }
    }

    private func parseThemeName(from filePath: String) -> String? {
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else { return nil }
        let pattern = "Theme Name:\\s*(.+)"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive),
              let match = regex.firstMatch(in: content, range: NSRange(content.startIndex..., in: content)),
              let range = Range(match.range(at: 1), in: content) else { return nil }
        return String(content[range]).trimmingCharacters(in: .whitespaces)
    }

    private func parsePluginName(from filePath: String) -> String? {
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else { return nil }
        let pattern = "Plugin Name:\\s*(.+)"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive),
              let match = regex.firstMatch(in: content, range: NSRange(content.startIndex..., in: content)),
              let range = Range(match.range(at: 1), in: content) else { return nil }
        return String(content[range]).trimmingCharacters(in: .whitespaces)
    }

    private func validate() -> Bool {
        guard FileManager.default.fileExists(atPath: localPath) else {
            validationError = "Path does not exist"
            return false
        }

        guard !remotePath.isEmpty else {
            validationError = "Remote path is required"
            return false
        }

        guard !buildArtifact.isEmpty else {
            validationError = "Build artifact path is required"
            return false
        }

        validationError = nil
        return true
    }

    private func save() {
        let component = ComponentConfiguration(
            id: id,
            name: name,
            localPath: localPath,
            remotePath: remotePath,
            buildArtifact: buildArtifact,
            versionFile: versionFile.isEmpty ? nil : versionFile,
            versionPattern: versionPattern.isEmpty ? nil : versionPattern,
            isNetwork: isNetwork ? true : nil
        )

        Task {
            // Save the component to its standalone file
            try? await config.saveComponent(component)

            // Add new component reference if not already present
            if !isEditing {
                try? await config.updateActiveProject { project in
                    if !project.componentIds.contains(id) {
                        project.componentIds.append(id)
                    }
                }
            }
        }
    }
}
