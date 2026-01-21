import Foundation

/// Generic deployment service for uploading and extracting build artifacts to remote servers.
/// Supports both .zip and .tar.gz archives with atomic deploy (extract to temp, then swap).
class DeploymentService {
    private let ssh: SSHService
    private let basePath: String
    private let pathResolver: RemotePathResolver

    init?(project: ProjectConfiguration) {
        guard let serverId = project.serverId,
              let server = ConfigurationManager.readServer(id: serverId),
              let resolver = RemotePathResolver(project: project),
              let ssh = SSHService(server: server, basePath: resolver.basePath) else {
            return nil
        }
        self.ssh = ssh
        self.basePath = resolver.basePath
        self.pathResolver = resolver
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
        guard let buildArtifact = component.buildArtifact,
              let localArtifact = component.buildArtifactPath else {
            onComplete(.failure(DeploymentError.artifactNotFound("No build artifact configured for \(component.name)")))
            return
        }

        let remoteParent = pathResolver.componentParent(for: component)
        let artifactName = RemotePathResolver.filename(of: buildArtifact)
        let stagingPath = pathResolver.stagingUploadPath(for: component)
        let tempDir = pathResolver.tempDeployPath(for: component)

        // Determine extraction command based on file type (extract from staging path)
        let extractCommand: String
        switch component.artifactExtension {
        case "zip":
            extractCommand = "unzip -o ~/'\(stagingPath)' -d '\(tempDir)'"
        case "gz", "tgz":
            extractCommand = "mkdir -p '\(tempDir)' && tar -xzf ~/'\(stagingPath)' -C '\(tempDir)'"
        default:
            onComplete(.failure(DeploymentError.unsupportedArtifactType(component.artifactExtension)))
            return
        }

        // Ensure staging directory exists, then upload artifact
        onOutput?("> Uploading \(artifactName)...\n")
        ssh.executeCommand("mkdir -p ~/tmp") { [weak self] _ in
            guard let self = self else { return }

            self.ssh.uploadFile(localPath: localArtifact, remotePath: stagingPath, onOutput: onOutput) { [weak self] result in
                guard let self = self else { return }

                switch result {
                case .success:
                    onOutput?("> Upload complete. Extracting...\n")

                    // Atomic deploy: extract from staging to temp, fix permissions, swap, cleanup
                    let deployCommand = """
                        cd '\(remoteParent)' && \
                        rm -rf '\(tempDir)' && \
                        \(extractCommand) && \
                        find '\(tempDir)/\(component.id)' -type f -exec chmod 644 {} \\; && \
                        find '\(tempDir)/\(component.id)' -type d -exec chmod 755 {} \\; && \
                        rm -rf '\(component.id)' && \
                        mv '\(tempDir)/\(component.id)' '\(component.id)' && \
                        rm -rf '\(tempDir)' && \
                        rm ~/'\(stagingPath)'
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
    }

    /// Synchronous deployment wrapper for CLI use.
    func deploySync(
        component: DeployableComponent,
        onOutput: ((String) -> Void)? = nil
    ) async throws {
        try await withCheckedThrowingContinuation { continuation in
            deploy(component: component, onOutput: onOutput) { result in
                switch result {
                case .success:
                    continuation.resume()
                case .failure(let error):
                    continuation.resume(throwing: error)
                }
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
        let remoteDir = pathResolver.componentDirectory(for: component)

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
                if let versionFilePath = self.pathResolver.versionFilePath(for: component) {
                    self.fetchVersionFromFile(
                        remotePath: versionFilePath,
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
                    onComplete(.success(.parseError("Version pattern did not match in \(remotePath)")))
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

    /// Fetch versions for multiple components with throttled SSH connections
    func fetchRemoteVersions(
        components: [DeployableComponent],
        onComplete: @escaping (Result<[String: VersionInfo], Error>) -> Void
    ) {
        let maxConcurrent = 3
        var results: [String: VersionInfo] = [:]
        let resultsLock = NSLock()
        let semaphore = DispatchSemaphore(value: maxConcurrent)
        let group = DispatchGroup()

        for component in components {
            group.enter()
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                semaphore.wait()
                self?.fetchRemoteVersion(component: component) { result in
                    defer {
                        semaphore.signal()
                        group.leave()
                    }
                    resultsLock.lock()
                    switch result {
                    case .success(let versionInfo):
                        results[component.id] = versionInfo
                    case .failure(let error):
                        results[component.id] = .parseError(error.localizedDescription)
                    }
                    resultsLock.unlock()
                }
            }
        }

        group.notify(queue: .main) {
            onComplete(.success(results))
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
