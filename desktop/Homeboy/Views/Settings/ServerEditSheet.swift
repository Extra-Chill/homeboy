import SwiftUI

import Foundation

struct ServerEditSheet: View {
    @ObservedObject var config: ConfigurationManager
    @Environment(\.dismiss) private var dismiss
    
    let existingServer: ServerConfig?
    let onSave: (ServerConfig) -> Void
    let onDelete: (() -> Void)?
    
    @State private var serverId: String = ""
    @State private var serverName: String = ""
    @State private var host: String = ""
    @State private var username: String = ""
    @State private var port: String = "22"
    
    @State private var hasSSHKey = false
    @State private var publicKey: String?
    @State private var isGeneratingKey = false
    @State private var isTestingConnection = false
    @State private var selectedConnectionTestProjectId: String = ""
    @State private var connectionTestResult: (success: Bool, message: String)?
    
    @State private var showDeleteConfirmation = false
    @State private var deleteConfirmText = ""
    
    private var isNewServer: Bool { existingServer == nil }
    
    private var canSave: Bool {
        !serverId.isEmpty && !serverName.isEmpty && !host.isEmpty && !username.isEmpty
    }
    
    private var projectsUsingServer: [ProjectConfiguration] {
        guard let server = existingServer else { return [] }
        return config.availableProjectIds()
            .compactMap { config.loadProject(id: $0) }
            .filter { $0.serverId == server.id }
    }
    
    init(config: ConfigurationManager, server: ServerConfig? = nil, onSave: @escaping (ServerConfig) -> Void, onDelete: (() -> Void)? = nil) {
        self.config = config
        self.existingServer = server
        self.onSave = onSave
        self.onDelete = onDelete
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text(isNewServer ? "Add Server" : "Edit Server")
                    .font(.headline)
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.escape)
            }
            .padding()
            
            Divider()
            
            // Content
            Form {
                Section("Server Details") {
                    if isNewServer {
                        TextField("Server ID", text: $serverId)
                            .textFieldStyle(.roundedBorder)
                        Text("Unique identifier (e.g., production-1, staging)")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    } else {
                        LabeledContent("Server ID", value: serverId)
                    }
                    
                    TextField("Display Name", text: $serverName)
                        .textFieldStyle(.roundedBorder)
                    
                    TextField("Host", text: $host)
                        .textFieldStyle(.roundedBorder)
                    Text("SSH host (e.g., example.com or 203.0.113.10)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    TextField("Username", text: $username)
                        .textFieldStyle(.roundedBorder)
                    
                    TextField("Port", text: $port)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 80)
                }
                
                Section("SSH Key") {
                    if hasSSHKey {
                        HStack {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundColor(.green)
                            Text("SSH key configured")
                            Spacer()
                            Button("Show") { showPublicKey() }
                                .buttonStyle(.borderless)
                            Button("Regenerate") { generateSSHKey() }
                                .buttonStyle(.borderless)
                        }
                    } else {
                        Text("No SSH key configured for this server.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        
                        Button("Generate SSH Key") { generateSSHKey() }
                            .disabled(isGeneratingKey || serverId.isEmpty)
                        
                        if isGeneratingKey {
                            ProgressView()
                                .controlSize(.small)
                        }
                    }
                    
                    if let key = publicKey {
                        GroupBox("Public Key (add to ~/.ssh/authorized_keys on server)") {
                            ScrollView {
                                Text(key)
                                    .font(.system(.caption, design: .monospaced))
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .frame(height: 60)
                            
                            Button("Copy to Clipboard") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(key, forType: .string)
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                }
                
                 Section("Connection Test") {
                     if projectsUsingServer.isEmpty {
                         Text("Link this server to a project to test via the Homeboy CLI.")
                             .font(.caption)
                             .foregroundColor(.secondary)
                     } else {
                         Picker("Project", selection: $selectedConnectionTestProjectId) {
                             ForEach(projectsUsingServer) { project in
                                 let isActive = project.id == config.activeProject?.id
                                 Text(isActive ? "\(project.name) (Active)" : project.name)
                                     .tag(project.id)
                             }
                         }
                         Text("Project context used for CLI SSH test")
                             .font(.caption)
                             .foregroundColor(.secondary)
                     }

                     HStack {
                         Button("Test SSH Connection") { testConnection() }
                             .disabled(!hasSSHKey || isTestingConnection || selectedConnectionTestProjectId.isEmpty)

                         if isTestingConnection {
                             ProgressView()
                                 .controlSize(.small)
                         }
                     }

                     if let result = connectionTestResult {
                         if result.success {
                             HStack {
                                 Image(systemName: "checkmark.circle.fill")
                                     .foregroundColor(.green)
                                 Text(result.message)
                                     .font(.caption)
                                     .foregroundColor(.green)
                             }
                         } else {
                             InlineErrorView(result.message, source: "SSH Connection Test")
                         }
                     }
                 }
                
                if !isNewServer {
                    Section("Danger Zone") {
                        if projectsUsingServer.isEmpty {
                            Button("Delete Server", role: .destructive) {
                                showDeleteConfirmation = true
                            }
                        } else {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Cannot delete - used by \(projectsUsingServer.count) project(s):")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                                ForEach(projectsUsingServer) { project in
                                    Text("• \(project.name)")
                                        .font(.caption)
                                        .foregroundColor(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            .formStyle(.grouped)
            
            Divider()
            
            // Footer
            HStack {
                Spacer()
                Button("Save") { saveServer() }
                    .keyboardShortcut(.return)
                    .disabled(!canSave)
            }
            .padding()
        }
        .frame(width: 500, height: 600)
        .onAppear { loadExistingServer() }
        .onChange(of: selectedConnectionTestProjectId) { _, _ in
            connectionTestResult = nil
        }
        .alert("Delete Server", isPresented: $showDeleteConfirmation) {
            TextField("Type server name to confirm", text: $deleteConfirmText)
            Button("Cancel", role: .cancel) { }
            Button("Delete", role: .destructive) {
                if deleteConfirmText == serverName {
                    onDelete?()
                    dismiss()
                }
            }
            .disabled(deleteConfirmText != serverName)
        } message: {
            Text("Type \"\(serverName)\" to confirm deletion.")
        }
    }
    
    private func loadExistingServer() {
        if let server = existingServer {
            serverId = server.id
            serverName = server.name
            host = server.host
            username = server.user
            port = String(server.port)

            selectedConnectionTestProjectId = projectsUsingServer.first?.id ?? ""

            Task {
                await refreshKeyStatusFromCLI()
            }
        }
    }
    
    private func saveServer() {
        let server = ServerConfig(
            id: serverId,
            name: serverName,
            host: host,
            user: username,
            port: Int(port) ?? 22
        )
        onSave(server)
        dismiss()
    }
    
    private func generateSSHKey() {
        guard !serverId.isEmpty else { return }

        isGeneratingKey = true
        publicKey = nil

        Task {
            do {
                _ = try await HomeboyCLI.shared.serverKeyGenerate(serverId: serverId)
                await refreshKeyStatusFromCLI()
            } catch {
                isGeneratingKey = false
                connectionTestResult = (false, "Key generation failed: \(error.localizedDescription)")
            }
        }
    }

    private func showPublicKey() {
        Task {
            do {
                let response = try await HomeboyCLI.shared.serverKeyShow(serverId: serverId)
                let key = response.output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines)
                if !key.isEmpty {
                    publicKey = key
                }
            } catch {
                connectionTestResult = (false, error.localizedDescription)
            }
        }
    }

    private func refreshKeyStatusFromCLI() async {
        defer {
            isGeneratingKey = false
        }

        guard !serverId.isEmpty else {
            hasSSHKey = false
            publicKey = nil
            return
        }

        do {
            let response = try await HomeboyCLI.shared.serverKeyShow(serverId: serverId)
            let key = response.output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines)
            hasSSHKey = !key.isEmpty
            publicKey = key.isEmpty ? nil : key
        } catch {
            hasSSHKey = false
            publicKey = nil
        }
    }
    
    private func testConnection() {
        isTestingConnection = true
        connectionTestResult = nil

        guard !selectedConnectionTestProjectId.isEmpty else {
            isTestingConnection = false
            connectionTestResult = (false, "Select a project to test the connection.")
            return
        }

        Task {
            do {
                let response = try await HomeboyCLI.shared.sshCommand(projectId: selectedConnectionTestProjectId, command: "echo 'Connection successful'")

                isTestingConnection = false
                let output = response.output.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines)

                if response.exitCode == 0 {
                    connectionTestResult = (true, output.isEmpty ? "Connection successful" : output)
                } else {
                    connectionTestResult = (false, output.isEmpty ? "SSH connection failed" : output)
                }
            } catch {
                isTestingConnection = false
                connectionTestResult = (false, error.localizedDescription)
            }
        }
    }
}
