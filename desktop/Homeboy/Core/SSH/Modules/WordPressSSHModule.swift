import Foundation

/// WordPress-specific SSH operations for deployment and version management
class WordPressSSHModule {
    private let ssh: SSHService
    private let wpContentPath: String
    
    /// Path to themes directory
    var themesPath: String { "\(wpContentPath)/themes" }
    
    /// Path to plugins directory
    var pluginsPath: String { "\(wpContentPath)/plugins" }
    
    // MARK: - Initialization
    
    /// Initialize with an SSHService and wp-content path
    init?(ssh: SSHService, wpContentPath: String) {
        guard !wpContentPath.isEmpty else { return nil }
        self.ssh = ssh
        self.wpContentPath = wpContentPath
    }
    
    /// Initialize from active project configuration
    init?() {
        let project = ConfigurationManager.readCurrentProject()
        
        guard project.projectType == .wordpress,
              let wordpress = project.wordpress,
              wordpress.isConfigured,
              let sshService = SSHService() else {
            return nil
        }
        
        self.ssh = sshService
        self.wpContentPath = wordpress.wpContentPath
    }
    
    // MARK: - Validation
    
    /// Validate that the wp-content path is a valid WordPress wp-content directory
    func validateWPContentPath() async throws -> Bool {
        // Check for themes and plugins directories
        let themesExists = try await ssh.isDirectory(themesPath)
        let pluginsExists = try await ssh.isDirectory(pluginsPath)
        
        return themesExists && pluginsExists
    }
    
    /// Get validation status with details
    func getValidationStatus() async -> WPContentValidationStatus {
        do {
            let themesExists = try await ssh.isDirectory(themesPath)
            let pluginsExists = try await ssh.isDirectory(pluginsPath)
            
            if themesExists && pluginsExists {
                return .valid
            } else if !themesExists && !pluginsExists {
                return .invalid("Missing themes/ and plugins/ directories")
            } else if !themesExists {
                return .invalid("Missing themes/ directory")
            } else {
                return .invalid("Missing plugins/ directory")
            }
        } catch {
            return .error(error.localizedDescription)
        }
    }
    
    // MARK: - Version Detection
    
    /// Fetch remote versions for deployable components
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
        
        ssh.executeCommand(command) { result in
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
    
    // MARK: - Deployment
    
    /// Deploy a component to the remote server
    func deployComponent(
        _ component: DeployableComponent,
        buildPath: String,
        onOutput: ((String) -> Void)? = nil,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        let remotePath = component.type == .theme ? themesPath : pluginsPath
        let remoteZipPath = "\(remotePath)/\(component.id).zip"
        
        // Upload zip file
        ssh.uploadFile(localPath: buildPath, remotePath: remoteZipPath, onOutput: onOutput) { [weak self] result in
            guard let self = self else { return }
            
            switch result {
            case .success:
                // Unzip and cleanup
                let unzipCommand = """
                    cd '\(remotePath)' && \
                    rm -rf '\(component.id)' && \
                    unzip -o '\(component.id).zip' && \
                    rm '\(component.id).zip'
                    """
                
                self.ssh.executeCommand(unzipCommand, onOutput: onOutput) { unzipResult in
                    switch unzipResult {
                    case .success:
                        onComplete(.success(()))
                    case .failure(let error):
                        onComplete(.failure(error))
                    }
                }
                
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
    
    // MARK: - WP-CLI
    
    /// Execute a WP-CLI command on the remote server
    func executeWPCLI(
        _ command: String,
        onOutput: ((String) -> Void)? = nil,
        onComplete: @escaping (Result<String, Error>) -> Void
    ) {
        // Navigate to WordPress root (parent of wp-content) and run wp command
        let wpRoot = (wpContentPath as NSString).deletingLastPathComponent
        let fullCommand = "cd '\(wpRoot)' && wp \(command)"
        
        ssh.executeCommand(fullCommand, onOutput: onOutput, onComplete: onComplete)
    }
    
    /// Get WordPress version via WP-CLI
    func getWordPressVersion(onComplete: @escaping (Result<String, Error>) -> Void) {
        executeWPCLI("core version") { result in
            switch result {
            case .success(let output):
                onComplete(.success(output.trimmingCharacters(in: .whitespacesAndNewlines)))
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
}

// MARK: - Validation Status

enum WPContentValidationStatus {
    case valid
    case invalid(String)
    case error(String)
    
    var isValid: Bool {
        if case .valid = self { return true }
        return false
    }
    
    var message: String {
        switch self {
        case .valid: return "Valid wp-content directory"
        case .invalid(let msg): return msg
        case .error(let msg): return "Error: \(msg)"
        }
    }
}
