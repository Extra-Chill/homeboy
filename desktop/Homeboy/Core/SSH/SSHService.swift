import Foundation

enum SSHError: LocalizedError {
    case noCredentials
    case noSSHKey
    case connectionFailed(String)
    case commandFailed(String)
    case uploadFailed(String)
    case keyGenerationFailed(String)
    
    var errorDescription: String? {
        switch self {
        case .noCredentials: return "Server credentials not configured"
        case .noSSHKey: return "SSH key not configured"
        case .connectionFailed(let msg): return "Connection failed: \(msg)"
        case .commandFailed(let msg): return "Command failed: \(msg)"
        case .uploadFailed(let msg): return "Upload failed: \(msg)"
        case .keyGenerationFailed(let msg): return "Key generation failed: \(msg)"
        }
    }
}

class SSHService: ObservableObject {
    private let host: String
    private let username: String
    private let port: Int
    private let privateKeyPath: String
    private let serverId: String
    
    /// Optional base path for project operations (not required for browsing)
    let basePath: String?
    
    // MARK: - Configuration Check
    
    /// Check if active project has valid SSH configuration (server + SSH key)
    static func isConfigured() -> Bool {
        let project = ConfigurationManager.readCurrentProject()
        guard let serverId = project.serverId,
              let server = ConfigurationManager.readServer(id: serverId),
              !server.host.isEmpty,
              !server.user.isEmpty else {
            return false
        }
        return KeychainService.hasSSHKey(forServer: serverId)
    }
    
    /// Check if active project has full deployment configuration (server + SSH key + wp-content path)
    static func isConfiguredForWordPressDeployment() -> Bool {
        let project = ConfigurationManager.readCurrentProject()
        guard project.projectType == .wordpress,
              let wordpress = project.wordpress,
              wordpress.isConfigured else {
            return false
        }
        return isConfigured()
    }
    
    /// Check if a specific server has valid SSH configuration
    static func isConfigured(forServer serverId: String) -> Bool {
        guard let server = ConfigurationManager.readServer(id: serverId),
              !server.host.isEmpty,
              !server.user.isEmpty else {
            return false
        }
        return KeychainService.hasSSHKey(forServer: serverId)
    }
    
    // MARK: - Initializers
    
    /// Initialize with a ServerConfig and optional base path
    init?(server: ServerConfig, basePath: String? = nil) {
        guard !server.host.isEmpty, !server.user.isEmpty else {
            return nil
        }
        
        self.host = server.host
        self.username = server.user
        self.port = server.port
        self.basePath = basePath
        self.serverId = server.id
        self.privateKeyPath = SSHService.keyPath(forServer: server.id)
    }
    
    /// Initialize from active project configuration
    init?() {
        let projectConfig = ConfigurationManager.readCurrentProject()
        
        guard let serverId = projectConfig.serverId,
              let server = ConfigurationManager.readServer(id: serverId),
              !server.host.isEmpty,
              !server.user.isEmpty else {
            return nil
        }
        
        self.host = server.host
        self.username = server.user
        self.port = server.port
        self.basePath = projectConfig.basePath
        self.serverId = serverId
        self.privateKeyPath = SSHService.keyPath(forServer: serverId)
    }
    
    // MARK: - Legacy Key Path (CLI Compatibility)
    
    /// Default key path for legacy/CLI usage
    static var defaultKeyPath: String {
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy/id_rsa").path
    }
    
    /// Ensure legacy SSH key file exists (for CLI commands)
    static func ensureKeyFileExists() -> Bool {
        FileManager.default.fileExists(atPath: defaultKeyPath)
    }
    
    // MARK: - Key Paths (Per-Server)
    
    /// Key path for a specific server
    static func keyPath(forServer serverId: String) -> String {
        KeychainService.sshKeyPath(forServer: serverId)
    }
    
    /// Public key path for a specific server
    static func publicKeyPath(forServer serverId: String) -> String {
        KeychainService.sshPublicKeyPath(forServer: serverId)
    }
    
    /// Keys directory
    static var keysDirectory: URL {
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy").appendingPathComponent("keys")
    }
    
    // MARK: - SSH Key Generation (Per-Server)
    
    /// Generate SSH key pair for a specific server
    static func generateSSHKeyPair(forServer serverId: String, onComplete: @escaping (Result<(privateKey: String, publicKey: String), Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                // Ensure keys directory exists
                try FileManager.default.createDirectory(at: keysDirectory, withIntermediateDirectories: true)
            } catch {
                DispatchQueue.main.async {
                    onComplete(.failure(SSHError.keyGenerationFailed("Failed to create keys directory: \(error.localizedDescription)")))
                }
                return
            }
            
            let keyPath = self.keyPath(forServer: serverId)
            let pubKeyPath = self.publicKeyPath(forServer: serverId)
            
            // Remove existing keys for this server
            try? FileManager.default.removeItem(atPath: keyPath)
            try? FileManager.default.removeItem(atPath: pubKeyPath)
            
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh-keygen")
            process.arguments = [
                "-t", "rsa",
                "-b", "4096",
                "-f", keyPath,
                "-N", "",  // Empty passphrase
                "-C", "homeboy-\(serverId)"
            ]
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            do {
                try process.run()
                process.waitUntilExit()
                
                if process.terminationStatus == 0 {
                    let privateKey = try String(contentsOfFile: keyPath, encoding: .utf8)
                    let publicKey = try String(contentsOfFile: pubKeyPath, encoding: .utf8)
                    
                    // Store in keychain (per-server)
                    try KeychainService.storeSSHKeyPair(forServer: serverId, privateKey: privateKey, publicKey: publicKey)
                    
                    // Set proper permissions on private key
                    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyPath)
                    
                    DispatchQueue.main.async {
                        onComplete(.success((privateKey, publicKey)))
                    }
                } else {
                    let data = pipe.fileHandleForReading.readDataToEndOfFile()
                    let output = String(data: data, encoding: .utf8) ?? "Unknown error"
                    DispatchQueue.main.async {
                        onComplete(.failure(SSHError.keyGenerationFailed(output)))
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    onComplete(.failure(SSHError.keyGenerationFailed(error.localizedDescription)))
                }
            }
        }
    }
    
    // MARK: - Ensure Key File Exists (Per-Server)
    
    /// Ensure SSH key file exists for a specific server (restores from keychain if missing)
    static func ensureKeyFileExists(forServer serverId: String) -> Bool {
        let keyPath = self.keyPath(forServer: serverId)
        
        // Check if file exists
        if FileManager.default.fileExists(atPath: keyPath) {
            return true
        }
        
        // Try to restore from keychain
        guard let privateKey = KeychainService.getSSHKeyPair(forServer: serverId).privateKey else {
            return false
        }
        
        do {
            try FileManager.default.createDirectory(at: keysDirectory, withIntermediateDirectories: true)
            try privateKey.write(toFile: keyPath, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyPath)
            return true
        } catch {
            return false
        }
    }
    
    /// Instance method: Ensure key file exists for this service's server
    func ensureKeyFileExists() -> Bool {
        SSHService.ensureKeyFileExists(forServer: serverId)
    }
    
    // MARK: - Connection Test
    
    func testConnection(onComplete: @escaping (Result<String, Error>) -> Void) {
        guard ensureKeyFileExists() else {
            onComplete(.failure(SSHError.noSSHKey))
            return
        }
        
        // If basePath is set, verify it exists; otherwise just test SSH connection
        let command = basePath.map { "echo 'Connection successful' && ls \($0)" } ?? "echo 'Connection successful'"
        
        executeCommand(command) { result in
            switch result {
            case .success(let output):
                onComplete(.success(output.trimmingCharacters(in: .whitespacesAndNewlines)))
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
    
    // MARK: - Execute SSH Command
    
    func executeCommand(
        _ command: String,
        onOutput: ((String) -> Void)? = nil,
        onComplete: @escaping (Result<String, Error>) -> Void
    ) {
        guard ensureKeyFileExists() else {
            onComplete(.failure(SSHError.noSSHKey))
            return
        }
        
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
            process.arguments = [
                "-i", privateKeyPath,
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "\(username)@\(host)",
                command
            ]
            
            let outputPipe = Pipe()
            let errorPipe = Pipe()
            process.standardOutput = outputPipe
            process.standardError = errorPipe
            
            var outputData = Data()
            var errorData = Data()
            
            outputPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    outputData.append(data)
                    if let line = String(data: data, encoding: .utf8) {
                        DispatchQueue.main.async {
                            onOutput?(line)
                        }
                    }
                }
            }
            
            errorPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    errorData.append(data)
                    if let line = String(data: data, encoding: .utf8) {
                        DispatchQueue.main.async {
                            onOutput?(line)
                        }
                    }
                }
            }
            
            do {
                try process.run()
                process.waitUntilExit()
                
                outputPipe.fileHandleForReading.readabilityHandler = nil
                errorPipe.fileHandleForReading.readabilityHandler = nil
                
                let output = String(data: outputData, encoding: .utf8) ?? ""
                let errorOutput = String(data: errorData, encoding: .utf8) ?? ""
                
                DispatchQueue.main.async {
                    if process.terminationStatus == 0 {
                        onComplete(.success(output))
                    } else {
                        onComplete(.failure(SSHError.commandFailed(errorOutput.isEmpty ? output : errorOutput)))
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    onComplete(.failure(SSHError.commandFailed(error.localizedDescription)))
                }
            }
        }
    }
    
    // MARK: - SCP Upload
    
    func uploadFile(
        localPath: String,
        remotePath: String,
        onOutput: ((String) -> Void)? = nil,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        guard ensureKeyFileExists() else {
            onComplete(.failure(SSHError.noSSHKey))
            return
        }
        
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/scp")
            process.arguments = [
                "-i", privateKeyPath,
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                localPath,
                "\(username)@\(host):\(remotePath)"
            ]
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            pipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if let line = String(data: data, encoding: .utf8), !line.isEmpty {
                    DispatchQueue.main.async {
                        onOutput?(line)
                    }
                }
            }
            
            do {
                try process.run()
                process.waitUntilExit()
                
                pipe.fileHandleForReading.readabilityHandler = nil
                
                DispatchQueue.main.async {
                    if process.terminationStatus == 0 {
                        onComplete(.success(()))
                    } else {
                        let data = pipe.fileHandleForReading.readDataToEndOfFile()
                        let output = String(data: data, encoding: .utf8) ?? "Upload failed"
                        onComplete(.failure(SSHError.uploadFailed(output)))
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    onComplete(.failure(SSHError.uploadFailed(error.localizedDescription)))
                }
            }
        }
    }
    
    // MARK: - Sync Command Execution (No Streaming)
    
    func executeCommandSync(_ command: String) async throws -> String {
        guard ensureKeyFileExists() else {
            throw SSHError.noSSHKey
        }
        
        // Capture values before entering @Sendable closure to avoid non-Sendable self capture
        let keyPath = privateKeyPath
        let sshHost = host
        let sshUsername = username
        
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
                process.arguments = [
                    "-i", keyPath,
                    "-o", "StrictHostKeyChecking=no",
                    "-o", "BatchMode=yes",
                    "\(sshUsername)@\(sshHost)",
                    command
                ]
                
                let pipe = Pipe()
                process.standardOutput = pipe
                process.standardError = pipe
                
                do {
                    try process.run()
                    process.waitUntilExit()
                    
                    let data = pipe.fileHandleForReading.readDataToEndOfFile()
                    let output = String(data: data, encoding: .utf8) ?? ""
                    
                    if process.terminationStatus == 0 {
                        continuation.resume(returning: output)
                    } else {
                        continuation.resume(throwing: SSHError.commandFailed(output))
                    }
                } catch {
                    continuation.resume(throwing: SSHError.commandFailed(error.localizedDescription))
                }
            }
        }
    }
    
    func uploadFileSync(localPath: String, remotePath: String) async throws -> String {
        guard ensureKeyFileExists() else {
            throw SSHError.noSSHKey
        }
        
        // Capture values before entering @Sendable closure to avoid non-Sendable self capture
        let keyPath = privateKeyPath
        let sshHost = host
        let sshUsername = username
        
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: "/usr/bin/scp")
                process.arguments = [
                    "-i", keyPath,
                    "-o", "StrictHostKeyChecking=no",
                    "-o", "BatchMode=yes",
                    localPath,
                    "\(sshUsername)@\(sshHost):\(remotePath)"
                ]
                
                let pipe = Pipe()
                process.standardOutput = pipe
                process.standardError = pipe
                
                do {
                    try process.run()
                    process.waitUntilExit()
                    
                    let data = pipe.fileHandleForReading.readDataToEndOfFile()
                    let output = String(data: data, encoding: .utf8) ?? ""
                    
                    if process.terminationStatus == 0 {
                        continuation.resume(returning: output)
                    } else {
                        continuation.resume(throwing: SSHError.uploadFailed(output))
                    }
                } catch {
                    continuation.resume(throwing: SSHError.uploadFailed(error.localizedDescription))
                }
            }
        }
    }
    
    // MARK: - File Browsing
    
    /// Get the SSH user's home directory
    func getHomeDirectory() async throws -> String {
        let output = try await executeCommandSync("echo $HOME")
        return output.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    
    /// List contents of a remote directory
    func listDirectory(_ path: String) async throws -> [RemoteFileEntry] {
        let output = try await executeCommandSync("ls -la '\(path)'")
        let lines = output.components(separatedBy: .newlines)
        
        var entries: [RemoteFileEntry] = []
        for line in lines {
            if let entry = RemoteFileEntry.parse(lsLine: line, basePath: path) {
                entries.append(entry)
            }
        }
        
        // Sort: directories first, then by name
        return entries.sorted { lhs, rhs in
            if lhs.isDirectory != rhs.isDirectory {
                return lhs.isDirectory
            }
            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }
    }
    
    /// Check if a remote file or directory exists
    func fileExists(_ path: String) async throws -> Bool {
        do {
            _ = try await executeCommandSync("test -e '\(path)' && echo 'exists'")
            return true
        } catch {
            return false
        }
    }
    
    /// Check if a remote path is a directory
    func isDirectory(_ path: String) async throws -> Bool {
        do {
            _ = try await executeCommandSync("test -d '\(path)' && echo 'directory'")
            return true
        } catch {
            return false
        }
    }
}
