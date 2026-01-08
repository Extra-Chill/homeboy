import SwiftUI

struct ServersSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    
    @State private var servers: [ServerConfig] = []
    @State private var isTestingConnection = false
    @State private var connectionTestResult: (success: Bool, message: String)?
    
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
        guard let serverId = config.activeProject.serverId else { return nil }
        return servers.first { $0.id == serverId }
    }
    
    private var hasSSHKey: Bool {
        guard let serverId = config.activeProject.serverId else { return false }
        return KeychainService.hasSSHKey(forServer: serverId)
    }
    
    private var isWordPressProject: Bool {
        config.activeProject.projectType == .wordpress
    }
    
    private var wpContentPath: String {
        config.activeProject.wordpress?.wpContentPath ?? ""
    }
    
    var body: some View {
        Form {
            Section("Server Connection") {
                if servers.isEmpty {
                    // No servers exist - show prominent add button
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundColor(.orange)
                            Text("No server configured")
                                .fontWeight(.medium)
                        }
                        
                        Text("A server connection is required for remote deployments, database access, and production WP-CLI commands.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        
                        Button("Add Server") {
                            serverToEdit = nil
                            showServerSheet = true
                        }
                    }
                } else {
                    // Server picker with Add/Edit buttons
                    HStack {
                        Picker("Server", selection: Binding(
                            get: { config.activeProject.serverId ?? "" },
                            set: { newValue in
                                if newValue == addNewServerTag {
                                    // Reset picker and open add sheet
                                    serverToEdit = nil
                                    showServerSheet = true
                                } else {
                                    config.activeProject.serverId = newValue.isEmpty ? nil : newValue
                                    config.saveActiveProject()
                                    connectionTestResult = nil
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
                            HStack {
                                Image(systemName: "exclamationmark.triangle.fill")
                                    .foregroundColor(.orange)
                                Text("No SSH key")
                                    .font(.caption)
                                Button("Configure") {
                                    serverToEdit = selectedServer
                                    showServerSheet = true
                                }
                                .buttonStyle(.borderless)
                                .font(.caption)
                            }
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
                                if config.activeProject.wordpress == nil {
                                    config.activeProject.wordpress = WordPressConfig()
                                }
                                config.activeProject.wordpress?.wpContentPath = newValue
                                config.saveActiveProject()
                                wpContentValidation = nil
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
            
            Section("Connection Test") {
                HStack {
                    Button("Test Connection") {
                        testServerConnection()
                    }
                    .disabled(!SSHService.isConfigured() || isTestingConnection)
                    
                    if isTestingConnection {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                
                if let result = connectionTestResult {
                    HStack {
                        Image(systemName: result.success ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundColor(result.success ? .green : .red)
                        Text(result.message)
                            .font(.caption)
                            .foregroundColor(result.success ? .green : .red)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { loadServers() }
        .sheet(isPresented: $showFileBrowser) {
            if let serverId = config.activeProject.serverId {
                RemoteFileBrowserView(serverId: serverId, mode: .selectPath) { selectedPath in
                    if config.activeProject.wordpress == nil {
                        config.activeProject.wordpress = WordPressConfig()
                    }
                    config.activeProject.wordpress?.wpContentPath = selectedPath
                    config.saveActiveProject()
                    wpContentValidation = nil
                    Task { await validateWPContentPath() }
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
                    if config.activeProject.serverId == server.id {
                        config.activeProject.serverId = nil
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
                    config.activeProject.serverId = newServer.id
                    config.saveActiveProject()
                    loadServers()
                }
            }
        }
    }
    
    private func loadServers() {
        servers = config.availableServers()
    }
    
    private func testServerConnection() {
        guard let sshService = SSHService() else {
            connectionTestResult = (false, "SSH not configured")
            return
        }
        
        isTestingConnection = true
        connectionTestResult = nil
        
        sshService.testConnection { result in
            isTestingConnection = false
            switch result {
            case .success(let output):
                connectionTestResult = (true, output)
            case .failure(let error):
                connectionTestResult = (false, error.localizedDescription)
            }
        }
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
}
