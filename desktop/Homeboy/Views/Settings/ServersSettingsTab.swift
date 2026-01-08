import SwiftUI

struct ServersSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    
    @State private var servers: [ServerConfig] = []
    
    // Server sheet state
    @State private var showServerSheet = false
    @State private var serverToEdit: ServerConfig?
    
    // File browser state
    @State private var showFileBrowser = false
    @State private var isValidatingWPContent = false
    @State private var wpContentValidation: WPContentValidationStatus?
    
    // Special tag for "Add New Server..." option
    private let addNewServerTag = "__add_new__"
    
    private var selectedServer: ServerConfig? {
        guard let serverId = config.safeActiveProject.serverId else { return nil }
        return servers.first { $0.id == serverId }
    }
    
    private var hasSSHKey: Bool {
        guard let serverId = config.safeActiveProject.serverId else { return false }
        return KeychainService.hasSSHKey(forServer: serverId)
    }
    
    private var isWordPressProject: Bool {
        config.safeActiveProject.isWordPress
    }
    
    private var wpContentPath: String {
        config.safeActiveProject.wordpress?.wpContentPath ?? ""
    }
    
    var body: some View {
        Form {
            Section("Server Connection") {
                if servers.isEmpty {
                    // No servers exist - show prominent add button
                    VStack(alignment: .leading, spacing: 12) {
                        InlineWarningView(
                            "No server configured",
                            source: "Server Settings",
                            actionLabel: "Add Server"
                        ) {
                            serverToEdit = nil
                            showServerSheet = true
                        }
                        
                        Text("A server connection is required for remote deployments, database access, and production WP-CLI commands.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                } else {
                    // Server picker with Add/Edit buttons
                    HStack {
                        Picker("Server", selection: Binding(
                            get: { config.safeActiveProject.serverId ?? "" },
                            set: { newValue in
                                if newValue == addNewServerTag {
                                    // Reset picker and open add sheet
                                    serverToEdit = nil
                                    showServerSheet = true
                                } else {
                                    config.activeProject?.serverId = newValue.isEmpty ? nil : newValue
                                    config.saveActiveProject()
                                }
                            }
                        )) {
                            Text("Select a server...").tag("")
                            ForEach(servers) { server in
                                Text(server.name).tag(server.id)
                            }
                            Divider()
                            Text("Add New Server...").tag(addNewServerTag)
                        }
                        
                        if selectedServer != nil {
                            Button("Edit") {
                                serverToEdit = selectedServer
                                showServerSheet = true
                            }
                            .buttonStyle(.borderless)
                        } else {
                            Button("Add") {
                                serverToEdit = nil
                                showServerSheet = true
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                    
                    // Server details (when selected)
                    if let server = selectedServer {
                        LabeledContent("Host", value: server.host)
                        HStack {
                            Text("User")
                                .foregroundColor(.secondary)
                            Spacer()
                            Text("\(server.user) • Port \(server.port)")
                        }
                        
                        if hasSSHKey {
                            HStack {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundColor(.green)
                                Text("SSH key configured")
                                    .font(.caption)
                            }
                        } else {
                            InlineWarningView(
                                "No SSH key configured",
                                source: "Server Settings",
                                actionLabel: "Configure"
                            ) {
                                serverToEdit = selectedServer
                                showServerSheet = true
                            }
                            .font(.caption)
                        }
                    } else {
                        Text("Select a server to enable remote operations.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
            }
            
            // WordPress-specific: wp-content path picker
            if isWordPressProject {
                Section("WordPress Deployment") {
                    HStack {
                        TextField("wp-content path", text: Binding(
                            get: { wpContentPath },
                            set: { newValue in
                                if config.activeProject?.wordpress == nil {
                                    config.activeProject?.wordpress = WordPressConfig()
                                }
                                config.activeProject?.wordpress?.wpContentPath = newValue
                                config.saveActiveProject()
                                wpContentValidation = nil
                                
                                // Resolve symlinks for SCP compatibility
                                if let serverId = config.safeActiveProject.serverId {
                                    Task {
                                        let canonicalPath = await resolveCanonicalPath(newValue, serverId: serverId)
                                        if canonicalPath != newValue {
                                            config.activeProject?.wordpress?.wpContentPath = canonicalPath
                                            config.saveActiveProject()
                                        }
                                    }
                                }
                            }
                        ))
                        .textFieldStyle(.roundedBorder)
                        
                        Button("Browse") {
                            showFileBrowser = true
                        }
                        .disabled(selectedServer == nil || !hasSSHKey)
                    }
                    
                    // Validation status
                    if isValidatingWPContent {
                        HStack {
                            ProgressView()
                                .controlSize(.small)
                            Text("Validating...")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    } else if let validation = wpContentValidation {
                        HStack {
                            Image(systemName: validation.isValid ? "checkmark.circle.fill" : "xmark.circle.fill")
                                .foregroundColor(validation.isValid ? .green : .red)
                            Text(validation.message)
                                .font(.caption)
                                .foregroundColor(validation.isValid ? .green : .red)
                        }
                    } else if !wpContentPath.isEmpty {
                        Button("Validate wp-content") {
                            Task { await validateWPContentPath() }
                        }
                        .font(.caption)
                        .disabled(selectedServer == nil || !hasSSHKey)
                    }
                    
                    Text("Path to the wp-content directory on the remote server. Use Browse to select the folder.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
            // Non-WordPress: generic base path picker
            if !isWordPressProject {
                Section("Remote Deployment") {
                    HStack {
                        TextField("Base Path", text: Binding(
                            get: { config.safeActiveProject.basePath ?? "" },
                            set: { newValue in
                                config.activeProject?.basePath = newValue.isEmpty ? nil : newValue
                                config.saveActiveProject()
                                
                                // Resolve symlinks for SCP compatibility
                                if let serverId = config.safeActiveProject.serverId, !newValue.isEmpty {
                                    Task {
                                        let canonicalPath = await resolveCanonicalPath(newValue, serverId: serverId)
                                        if canonicalPath != newValue {
                                            config.activeProject?.basePath = canonicalPath
                                            config.saveActiveProject()
                                        }
                                    }
                                }
                            }
                        ))
                        .textFieldStyle(.roundedBorder)
                        
                        Button("Browse") {
                            showFileBrowser = true
                        }
                        .disabled(selectedServer == nil || !hasSSHKey)
                    }
                    
                    Text("Path to the project directory on the remote server where components will be deployed.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
        }
        .formStyle(.grouped)
        .onAppear { loadServers() }
        .sheet(isPresented: $showFileBrowser) {
            if let serverId = config.safeActiveProject.serverId {
                RemoteFileBrowserView(serverId: serverId, mode: .selectPath) { selectedPath in
                    Task {
                        // Resolve symlinks to get canonical path for SCP compatibility
                        let canonicalPath = await resolveCanonicalPath(selectedPath, serverId: serverId)
                        
                        if isWordPressProject {
                            if config.activeProject?.wordpress == nil {
                                config.activeProject?.wordpress = WordPressConfig()
                            }
                            config.activeProject?.wordpress?.wpContentPath = canonicalPath
                            config.saveActiveProject()
                            wpContentValidation = nil
                            await validateWPContentPath()
                        } else {
                            config.activeProject?.basePath = canonicalPath
                            config.saveActiveProject()
                        }
                    }
                }
            }
        }
        .sheet(isPresented: $showServerSheet, onDismiss: { loadServers() }) {
            if let server = serverToEdit {
                // Edit existing server
                ServerEditSheet(config: config, server: server) { updatedServer in
                    config.saveServer(updatedServer)
                    loadServers()
                } onDelete: {
                    // Clear selection if we deleted the active server
                    if config.safeActiveProject.serverId == server.id {
                        config.activeProject?.serverId = nil
                        config.saveActiveProject()
                    }
                    config.deleteServer(id: server.id)
                    KeychainService.clearSSHKeys(forServer: server.id)
                    loadServers()
                }
            } else {
                // Add new server
                ServerEditSheet(config: config) { newServer in
                    config.saveServer(newServer)
                    // Auto-select the new server
                    config.activeProject?.serverId = newServer.id
                    config.saveActiveProject()
                    loadServers()
                }
            }
        }
    }
    
    private func loadServers() {
        servers = config.availableServers()
    }
    
    private func validateWPContentPath() async {
        guard let wpModule = WordPressSSHModule() else {
            wpContentValidation = .error("SSH not configured")
            return
        }
        
        isValidatingWPContent = true
        wpContentValidation = nil
        
        let status = await wpModule.getValidationStatus()
        
        isValidatingWPContent = false
        wpContentValidation = status
    }
    
    /// Resolve symlinks in a remote path to get the canonical filesystem path.
    /// This ensures SCP uploads work correctly when paths contain symlinks.
    private func resolveCanonicalPath(_ path: String, serverId: String) async -> String {
        guard !path.isEmpty,
              let server = ConfigurationManager.readServer(id: serverId),
              let ssh = SSHService(server: server) else {
            return path
        }
        
        do {
            let resolved = try await ssh.executeCommandSync("readlink -f '\(path)'")
            return resolved.trimmingCharacters(in: .whitespacesAndNewlines)
        } catch {
            return path  // Fall back to original if resolution fails
        }
    }
}
