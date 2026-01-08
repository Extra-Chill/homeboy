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
        case .noCredentials: return "Cloudways credentials not configured"
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
    let appPath: String
    private let privateKeyPath: String
    
    var wpContentPath: String {
        "\(appPath)/wp-content"
    }
    
    init?(privateKeyPath: String? = nil) {
        let creds = KeychainService.getCloudwaysCredentials()
        guard let host = creds.host,
              let username = creds.username,
              let appPath = creds.appPath else {
            return nil
        }
        
        self.host = host
        self.username = username
        self.appPath = appPath
        
        if let path = privateKeyPath {
            self.privateKeyPath = path
        } else {
            self.privateKeyPath = SSHService.defaultKeyPath
        }
    }
    
    static var defaultKeyPath: String {
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let appDir = appSupport.appendingPathComponent("ExtraChillDesktop")
        return appDir.appendingPathComponent("id_rsa").path
    }
    
    static var defaultPublicKeyPath: String {
        return defaultKeyPath + ".pub"
    }
    
    // MARK: - SSH Key Generation
    
    static func generateSSHKeyPair(onComplete: @escaping (Result<(privateKey: String, publicKey: String), Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            let appDir = appSupport.appendingPathComponent("ExtraChillDesktop")
            
            do {
                try FileManager.default.createDirectory(at: appDir, withIntermediateDirectories: true)
            } catch {
                DispatchQueue.main.async {
                    onComplete(.failure(SSHError.keyGenerationFailed("Failed to create app directory: \(error.localizedDescription)")))
                }
                return
            }
            
            let keyPath = appDir.appendingPathComponent("id_rsa").path
            let pubKeyPath = keyPath + ".pub"
            
            // Remove existing keys
            try? FileManager.default.removeItem(atPath: keyPath)
            try? FileManager.default.removeItem(atPath: pubKeyPath)
            
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh-keygen")
            process.arguments = [
                "-t", "rsa",
                "-b", "4096",
                "-f", keyPath,
                "-N", "",  // Empty passphrase
                "-C", "extrachill-desktop"
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
                    
                    // Store in keychain
                    try KeychainService.storeSSHKeyPair(privateKey: privateKey, publicKey: publicKey)
                    
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
    
    static func ensureKeyFileExists() -> Bool {
        let keyPath = defaultKeyPath
        
        // Check if file exists
        if FileManager.default.fileExists(atPath: keyPath) {
            return true
        }
        
        // Try to restore from keychain
        guard let privateKey = KeychainService.getSSHKeyPair().privateKey else {
            return false
        }
        
        do {
            let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            let appDir = appSupport.appendingPathComponent("ExtraChillDesktop")
            try FileManager.default.createDirectory(at: appDir, withIntermediateDirectories: true)
            try privateKey.write(toFile: keyPath, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyPath)
            return true
        } catch {
            return false
        }
    }
    
    // MARK: - Connection Test
    
    func testConnection(onComplete: @escaping (Result<String, Error>) -> Void) {
        guard SSHService.ensureKeyFileExists() else {
            onComplete(.failure(SSHError.noSSHKey))
            return
        }
        
        executeCommand("echo 'Connection successful' && wp core version --path=\(appPath)") { result in
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
        guard SSHService.ensureKeyFileExists() else {
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
        guard SSHService.ensureKeyFileExists() else {
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
        guard SSHService.ensureKeyFileExists() else {
            throw SSHError.noSSHKey
        }
        
        return try await withCheckedThrowingContinuation { continuation in
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
        guard SSHService.ensureKeyFileExists() else {
            throw SSHError.noSSHKey
        }
        
        return try await withCheckedThrowingContinuation { continuation in
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
    
    // MARK: - Fetch Remote Versions
    
    func fetchRemoteVersions(
        components: [DeployableComponent],
        onComplete: @escaping (Result<[String: String], Error>) -> Void
    ) {
        var versionChecks: [String] = []
        
        for component in components {
            let remotePath = "\(wpContentPath)/\(component.remotePath)/\(component.mainFile)"
            if component.mainFile == "style.css" {
                versionChecks.append("echo '\(component.id):'$(grep -m1 'Version:' \"\(remotePath)\" 2>/dev/null | sed 's/.*Version:[[:space:]]*//' | tr -d '[:space:]*')")
            } else {
                versionChecks.append("echo '\(component.id):'$(grep -m1 'Version:' \"\(remotePath)\" 2>/dev/null | sed 's/.*Version:[[:space:]]*//' | tr -d '[:space:]')")
            }
        }
        
        let command = versionChecks.joined(separator: " && ")
        
        executeCommand(command) { result in
            switch result {
            case .success(let output):
                var versions: [String: String] = [:]
                let lines = output.components(separatedBy: .newlines)
                for line in lines {
                    let parts = line.split(separator: ":", maxSplits: 1)
                    if parts.count == 2 {
                        let id = String(parts[0])
                        let version = String(parts[1]).trimmingCharacters(in: .whitespaces)
                        if !version.isEmpty {
                            versions[id] = version
                        }
                    }
                }
                onComplete(.success(versions))
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
}
