import Foundation

/// Generic deployment service for uploading and extracting build artifacts to remote servers.
/// Supports both .zip and .tar.gz archives with atomic deploy (extract to temp, then swap).
class DeploymentService {
    private let ssh: SSHService
    private let basePath: String
    
    init?(project: ProjectConfiguration) {
        guard let serverId = project.serverId,
              let _ = ConfigurationManager.readServer(id: serverId),
              let basePath = project.basePath,
              !basePath.isEmpty,
              let ssh = SSHService() else {
            return nil
        }
        self.ssh = ssh
        self.basePath = basePath
    }
    
    // MARK: - Deployment
    
    /// Deploy a pre-built artifact to the remote server using atomic swap pattern.
    /// 1. Upload artifact
    /// 2. Extract to temp directory
    /// 3. Remove old version
    /// 4. Move new version into place
    /// 5. Cleanup
    func deploy(
        component: DeployableComponent,
        onOutput: ((String) -> Void)?,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        let localArtifact = component.buildArtifactPath
        let remoteDir = "\(basePath)/\(component.remotePath)".replacingOccurrences(of: "//", with: "/")
        let remoteParent = (remoteDir as NSString).deletingLastPathComponent
        let artifactName = (component.buildArtifact as NSString).lastPathComponent
        let remoteArtifact = "\(remoteParent)/\(artifactName)"
        let tempDir = "\(remoteParent)/__temp_deploy__"
        
        // Determine extraction command based on file type
        let extractCommand: String
        switch component.artifactExtension {
        case "zip":
            extractCommand = "unzip -o '\(remoteArtifact)' -d '\(tempDir)'"
        case "gz", "tgz":
            extractCommand = "mkdir -p '\(tempDir)' && tar -xzf '\(remoteArtifact)' -C '\(tempDir)'"
        default:
            onComplete(.failure(DeploymentError.unsupportedArtifactType(component.artifactExtension)))
            return
        }
        
        // Upload artifact
        onOutput?("> Uploading \(artifactName)...\n")
        ssh.uploadFile(localPath: localArtifact, remotePath: remoteArtifact, onOutput: onOutput) { [weak self] result in
            guard let self = self else { return }
            
            switch result {
            case .success:
                onOutput?("> Upload complete. Extracting...\n")
                
                // Atomic deploy: extract to temp, swap, cleanup
                let deployCommand = """
                    cd '\(remoteParent)' && \
                    rm -rf '\(tempDir)' && \
                    \(extractCommand) && \
                    rm -rf '\(component.id)' && \
                    mv '\(tempDir)/\(component.id)' '\(component.id)' && \
                    rm -rf '\(tempDir)' && \
                    rm '\(artifactName)'
                    """
                
                self.ssh.executeCommand(deployCommand, onOutput: onOutput) { deployResult in
                    switch deployResult {
                    case .success:
                        onOutput?("> Deploy complete.\n")
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
    
    // MARK: - Version Detection
    
    /// Fetch remote version for a component.
    /// Falls back to file timestamp if version detection isn't configured.
    func fetchRemoteVersion(
        component: DeployableComponent,
        onComplete: @escaping (Result<VersionInfo, Error>) -> Void
    ) {
        let remoteDir = "\(basePath)/\(component.remotePath)".replacingOccurrences(of: "//", with: "/")
        
        // First check if the remote directory exists
        ssh.executeCommand("test -d '\(remoteDir)' && echo 'exists' || echo 'missing'") { [weak self] result in
            guard let self = self else { return }
            
            switch result {
            case .success(let output):
                if output.trimmingCharacters(in: .whitespacesAndNewlines) == "missing" {
                    onComplete(.success(.notDeployed))
                    return
                }
                
                // Try version file detection if configured
                if let versionFile = component.versionFile {
                    self.fetchVersionFromFile(
                        remotePath: "\(remoteDir)/\(versionFile)",
                        pattern: component.versionPattern,
                        onComplete: onComplete
                    )
                } else {
                    // Fall back to directory timestamp
                    self.fetchDirectoryTimestamp(remotePath: remoteDir, onComplete: onComplete)
                }
                
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
    
    private func fetchVersionFromFile(
        remotePath: String,
        pattern: String?,
        onComplete: @escaping (Result<VersionInfo, Error>) -> Void
    ) {
        ssh.executeCommand("cat '\(remotePath)' 2>/dev/null || echo '__FILE_NOT_FOUND__'") { result in
            switch result {
            case .success(let content):
                if content.contains("__FILE_NOT_FOUND__") {
                    onComplete(.success(.notDeployed))
                    return
                }
                
                if let version = VersionParser.parseVersion(from: content, pattern: pattern) {
                    onComplete(.success(.version(version)))
                } else {
                    // Version file exists but couldn't parse - return unknown
                    onComplete(.success(.notDeployed))
                }
                
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
    
    private func fetchDirectoryTimestamp(
        remotePath: String,
        onComplete: @escaping (Result<VersionInfo, Error>) -> Void
    ) {
        // Get modification time in epoch seconds
        ssh.executeCommand("stat -c %Y '\(remotePath)' 2>/dev/null || stat -f %m '\(remotePath)' 2>/dev/null") { result in
            switch result {
            case .success(let output):
                let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
                if let timestamp = Double(trimmed) {
                    let date = Date(timeIntervalSince1970: timestamp)
                    onComplete(.success(.timestamp(date)))
                } else {
                    onComplete(.success(.notDeployed))
                }
                
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
    
    // MARK: - Batch Version Fetching
    
    /// Fetch versions for multiple components
    func fetchRemoteVersions(
        components: [DeployableComponent],
        onComplete: @escaping (Result<[String: VersionInfo], Error>) -> Void
    ) {
        var results: [String: VersionInfo] = [:]
        let group = DispatchGroup()
        var firstError: Error?
        
        for component in components {
            group.enter()
            fetchRemoteVersion(component: component) { result in
                switch result {
                case .success(let versionInfo):
                    results[component.id] = versionInfo
                case .failure(let error):
                    if firstError == nil {
                        firstError = error
                    }
                }
                group.leave()
            }
        }
        
        group.notify(queue: .main) {
            if let error = firstError, results.isEmpty {
                onComplete(.failure(error))
            } else {
                onComplete(.success(results))
            }
        }
    }
}

// MARK: - Deployment Errors

enum DeploymentError: LocalizedError {
    case artifactNotFound(String)
    case unsupportedArtifactType(String)
    case notConfigured
    
    var errorDescription: String? {
        switch self {
        case .artifactNotFound(let path):
            return "Build artifact not found at: \(path)"
        case .unsupportedArtifactType(let ext):
            return "Unsupported artifact type: .\(ext). Use .zip or .tar.gz"
        case .notConfigured:
            return "Deployment not configured. Check server and base path settings."
        }
    }
}
