import SwiftUI

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
                    HStack {
                        Button("Test SSH Connection") { testConnection() }
                            .disabled(!hasSSHKey || isTestingConnection || host.isEmpty)
                        
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
            hasSSHKey = SSHKeyManager.hasKeyFile(forServer: server.id)
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
        
        SSHService.generateSSHKeyPair(forServer: serverId) { result in
            isGeneratingKey = false
            switch result {
            case .success(let keys):
                hasSSHKey = true
                publicKey = keys.publicKey
            case .failure(let error):
                connectionTestResult = (false, "Key generation failed: \(error.localizedDescription)")
            }
        }
    }
    
    private func showPublicKey() {
        publicKey = try? SSHKeyManager.readPublicKey(forServer: serverId)
    }
    
    private func testConnection() {
        // Create a temporary server config for testing
        let testServer = ServerConfig(
            id: serverId,
            name: serverName,
            host: host,
            user: username,
            port: Int(port) ?? 22
        )
        
        // We need a base path to test - use a simple echo command
        isTestingConnection = true
        connectionTestResult = nil
        
        DispatchQueue.global(qos: .userInitiated).async {
            // Ensure key file exists
            guard SSHKeyManager.restoreFromKeychainIfNeeded(forServer: serverId) else {
                DispatchQueue.main.async {
                    isTestingConnection = false
                    connectionTestResult = (false, "SSH key not found")
                }
                return
            }

            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
            process.arguments = [
                "-i", SSHKeyManager.privateKeyPath(forServer: serverId),
                "-p", String(testServer.port),
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=10",
                "\(testServer.user)@\(testServer.host)",
                "echo 'Connection successful'"
            ]
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            do {
                try process.run()
                process.waitUntilExit()
                
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8) ?? ""
                
                DispatchQueue.main.async {
                    isTestingConnection = false
                    if process.terminationStatus == 0 {
                        connectionTestResult = (true, output.trimmingCharacters(in: .whitespacesAndNewlines))
                    } else {
                        connectionTestResult = (false, output.trimmingCharacters(in: .whitespacesAndNewlines))
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    isTestingConnection = false
                    connectionTestResult = (false, error.localizedDescription)
                }
            }
        }
    }
}
