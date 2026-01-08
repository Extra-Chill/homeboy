import SwiftUI
import AppKit

struct ComponentsSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    
    @State private var showingAddSheet = false
    @State private var editingComponent: ComponentConfig? = nil
    @State private var componentToDelete: ComponentConfig? = nil
    @State private var showDeleteConfirmation = false
    
    private var themes: [ComponentConfig] {
        config.activeProject.components.filter { $0.type == .theme }
    }
    
    private var networkPlugins: [ComponentConfig] {
        config.activeProject.components.filter { $0.type == .plugin && $0.isNetwork }
    }
    
    private var sitePlugins: [ComponentConfig] {
        config.activeProject.components.filter { $0.type == .plugin && !$0.isNetwork }
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
                
                if config.activeProject.components.isEmpty {
                    Text("No components configured. Add themes and plugins to deploy.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
            if !themes.isEmpty {
                Section("Themes") {
                    ForEach(themes) { component in
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
            
            if !networkPlugins.isEmpty {
                Section("Network Plugins") {
                    ForEach(networkPlugins) { component in
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
            
            if !sitePlugins.isEmpty {
                Section("Site Plugins") {
                    ForEach(sitePlugins) { component in
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
    
    private func deleteComponent(_ component: ComponentConfig) {
        config.activeProject.components.removeAll { $0.id == component.id }
        config.saveActiveProject()
    }
}

// MARK: - Component Row

struct ComponentRow: View {
    let component: ComponentConfig
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
    let existing: ComponentConfig?
    
    @State private var localPath: String = ""
    @State private var id: String = ""
    @State private var name: String = ""
    @State private var type: ComponentType = .plugin
    @State private var isNetwork: Bool = false
    @State private var validationError: String? = nil
    
    @Environment(\.dismiss) private var dismiss
    
    private var isEditing: Bool {
        existing != nil
    }
    
    private var canSave: Bool {
        !localPath.isEmpty && !id.isEmpty && !name.isEmpty
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
                        
                        Picker("Type", selection: $type) {
                            Text("Theme").tag(ComponentType.theme)
                            Text("Plugin").tag(ComponentType.plugin)
                        }
                        .pickerStyle(.segmented)
                        
                        if type == .plugin {
                            Toggle("Network Plugin", isOn: $isNetwork)
                        }
                    }
                }
                
                if let error = validationError {
                    Section {
                        Text(error)
                            .foregroundColor(.red)
                            .font(.caption)
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
        .frame(width: 500, height: 400)
        .onAppear {
            if let existing = existing {
                localPath = existing.localPath
                id = existing.id
                name = existing.name
                type = existing.type
                isNetwork = existing.isNetwork
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
        panel.message = "Select a theme or plugin folder"
        
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
        
        if FileManager.default.fileExists(atPath: styleCSS) {
            type = .theme
            name = VersionParser.parseThemeName(from: styleCSS) ?? slug.capitalized
        } else if FileManager.default.fileExists(atPath: mainPHP) {
            type = .plugin
            name = VersionParser.parsePluginName(from: mainPHP) ?? slug.capitalized
        } else {
            name = slug.capitalized
        }
    }
    
    private func validate() -> Bool {
        guard FileManager.default.fileExists(atPath: localPath) else {
            validationError = "Path does not exist"
            return false
        }
        
        let mainFile = type == .theme ? "style.css" : "\(id).php"
        let mainFilePath = "\(localPath)/\(mainFile)"
        
        guard FileManager.default.fileExists(atPath: mainFilePath) else {
            validationError = "Missing \(mainFile)"
            return false
        }
        
        guard let content = try? String(contentsOfFile: mainFilePath, encoding: .utf8),
              VersionParser.parseVersion(from: content) != nil else {
            validationError = "Could not parse version from \(mainFile)"
            return false
        }
        
        validationError = nil
        return true
    }
    
    private func save() {
        let component = ComponentConfig(
            id: id,
            name: name,
            type: type,
            localPath: localPath,
            isNetwork: type == .plugin ? isNetwork : false
        )
        
        if isEditing {
            if let index = config.activeProject.components.firstIndex(where: { $0.id == existing?.id }) {
                config.activeProject.components[index] = component
            }
        } else {
            config.activeProject.components.append(component)
        }
        
        config.saveActiveProject()
    }
}
