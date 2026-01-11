import SwiftUI

import Foundation

struct ServersSettingsTab: View {
    @ObservedObject var config: ConfigurationManager

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
        return config.availableServers.first { $0.id == serverId }
    }
    
    @State private var hasSSHKeyForSelectedServer: Bool = false

    private var hasSSHKey: Bool {
        guard selectedServer != nil else { return false }
        return hasSSHKeyForSelectedServer
    }
    
    private var isWordPressProject: Bool {
        config.safeActiveProject.isWordPress
    }

    private func refreshSSHKeyStatusFromCLI() async {
        guard let serverId = config.safeActiveProject.serverId, !serverId.isEmpty else {
            hasSSHKeyForSelectedServer = false
            return
        }

        do {
            let response = try await HomeboyCLI.shared.serverKeyShow(serverId: serverId)
            let key = response.output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines)
            hasSSHKeyForSelectedServer = !key.isEmpty
        } catch {
            hasSSHKeyForSelectedServer = false
        }
    }
    
    private var basePath: String {
        config.safeActiveProject.basePath ?? ""
    }
    
    var body: some View {
        Form {
            Section("Server Connection") {
                if config.availableServers.isEmpty {
                    // No config.availableServers exist - show prominent add button
                    VStack(alignment: .leading, spacing: 12) {
                        InlineWarningView(
                            "No server configured",
                            source: "Server Settings",
                            actionLabel: "Add Server"
                        ) {
                            serverToEdit = nil
                            showServerSheet = true
                        }
                        
                        Text("A server connection is required for remote deployments, database access, and production CLI commands.")
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
                                    config.updateActiveProject { $0.serverId = newValue.isEmpty ? nil : newValue }
                                }
                            }
                        )) {
                            Text("Select a server...").tag("")
                            ForEach(config.availableServers) { server in
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
            
            // Deployment path picker (WordPress or generic)
            Section(isWordPressProject ? "WordPress Deployment" : "Remote Deployment") {
                HStack {
                    TextField("Base Path", text: Binding(
                        get: { basePath },
                        set: { newValue in
                            config.updateActiveProject { $0.basePath = newValue.isEmpty ? nil : newValue }
                            wpContentValidation = nil
                            
                            // Resolve symlinks for SCP compatibility
                            if let serverId = config.safeActiveProject.serverId, !newValue.isEmpty {
                                Task {
                                    let canonicalPath = await resolveCanonicalPath(newValue, serverId: serverId)
                                    if canonicalPath != newValue {
                                        config.updateActiveProject { $0.basePath = canonicalPath }
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
                
                // WordPress-specific validation
                if isWordPressProject {
                    if isValidatingWPContent {
                        HStack {
                            ProgressView()
                                .controlSize(.small)
                            Text("Validating...")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    } else if let validation = wpContentValidation {
                        if validation.isValid {
                            HStack {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundColor(.green)
                                Text(validation.message)
                                    .font(.caption)
                                    .foregroundColor(.green)
                            }
                        } else {
                            InlineErrorView(validation.message, source: "WordPress Deployment", path: basePath) {
                                wpContentValidation = nil
                            }
                            .font(.caption)
                        }
                    } else if !basePath.isEmpty {
                        Button("Validate wp-content") {
                            Task { await validateWPContentPath() }
                        }
                        .font(.caption)
                        .disabled(selectedServer == nil || !hasSSHKey)
                    }
                    
                    Text("WordPress root directory on the remote server (contains wp-content, wp-admin, etc).")
                        .font(.caption)
                        .foregroundColor(.secondary)
                } else {
                    Text("Path to the project directory on the remote server where components will be deployed.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showFileBrowser) {
            if let serverId = config.safeActiveProject.serverId {
                RemoteFileBrowserView(projectId: serverId, mode: .selectPath) { selectedPath in
                    Task {
                        // Resolve symlinks to get canonical path for SCP compatibility
                        let canonicalPath = await resolveCanonicalPath(selectedPath, serverId: serverId)
                        
                        config.updateActiveProject { $0.basePath = canonicalPath }
                        
                        if isWordPressProject {
                            wpContentValidation = nil
                            await validateWPContentPath()
                        }
                    }
                }
            }
        }
        .sheet(isPresented: $showServerSheet) {
            if let server = serverToEdit {
                // Edit existing server
                ServerEditSheet(config: config, server: server) { updatedServer in
                    config.saveServer(updatedServer)
                } onDelete: {
                    // Clear selection if we deleted the active server
                    if config.safeActiveProject.serverId == server.id {
                        config.updateActiveProject { $0.serverId = nil }
                    }
                    config.deleteServer(id: server.id)
                }
            } else {
                // Add new server
                ServerEditSheet(config: config) { newServer in
                    config.saveServer(newServer)
                    // Auto-select the new server
                    config.updateActiveProject { $0.serverId = newServer.id }
                }
            }
        }
        .task {
            await refreshSSHKeyStatusFromCLI()
        }
        .onChange(of: config.safeActiveProject.serverId) { _, _ in
            Task {
                await refreshSSHKeyStatusFromCLI()
            }
        }
    }
    
    private func validateWPContentPath() async {
        let project = config.safeActiveProject

        guard project.isWordPress,
              !project.id.isEmpty,
              let basePath = project.basePath,
              !basePath.isEmpty else {
            wpContentValidation = .error("WordPress project not configured")
            return
        }

        let projectId = project.id

        isValidatingWPContent = true
        wpContentValidation = nil

        do {
            let themes = "\(basePath)/wp-content/themes"
            let plugins = "\(basePath)/wp-content/plugins"

            async let themesCheck = HomeboyCLI.shared.sshCommand(
                projectId: projectId,
                command: "test -d '\(themes)' && echo yes || echo no"
            )
            async let pluginsCheck = HomeboyCLI.shared.sshCommand(
                projectId: projectId,
                command: "test -d '\(plugins)' && echo yes || echo no"
            )

            let themesExists = (try await themesCheck).output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines) == "yes"
            let pluginsExists = (try await pluginsCheck).output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines) == "yes"

            if themesExists && pluginsExists {
                wpContentValidation = .valid
            } else if !themesExists && !pluginsExists {
                wpContentValidation = .invalid("Missing themes/ and plugins/ directories")
            } else if !themesExists {
                wpContentValidation = .invalid("Missing themes/ directory")
            } else {
                wpContentValidation = .invalid("Missing plugins/ directory")
            }
        } catch {
            wpContentValidation = .error(error.localizedDescription)
        }

        isValidatingWPContent = false
    }

    /// Resolve symlinks in a remote path to get the canonical filesystem path.
    private func resolveCanonicalPath(_ path: String, serverId: String) async -> String {
        let project = config.safeActiveProject

        guard !path.isEmpty,
              !project.id.isEmpty,
              project.serverId == serverId else {
            return path
        }

        do {
            let response = try await HomeboyCLI.shared.sshCommand(projectId: project.id, command: "readlink -f '\(path)'")
            let resolved = response.output.trimmingCharacters(in: .whitespacesAndNewlines)
            return resolved.isEmpty ? path : resolved
        } catch {
            return path
        }
    }
}
