import ArgumentParser
import Foundation

struct Deploy: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "deploy",
        abstract: "Upload pre-built components to production server",
        discussion: """
            Uploads pre-built components to the remote server.

            Usage:
              homeboy deploy <project> <component-id>...
              homeboy deploy <project> --all
              homeboy deploy <project> --outdated

            Examples:
              homeboy deploy extrachill my-plugin
              homeboy deploy extrachill --all
              homeboy deploy extrachill --outdated --dry-run

            Prerequisites:
              - Server must be configured and linked to project
              - SSH key must be set up in Homeboy.app
              - Build artifacts must exist (run build.sh first)

            See 'homeboy docs deploy' for full documentation.
            """
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Component IDs to deploy")
    var componentIds: [String] = []
    
    @Flag(name: .long, help: "Deploy all configured components")
    var all: Bool = false
    
    @Flag(name: .long, help: "Deploy only components where local != remote version")
    var outdated: Bool = false
    
    @Flag(name: .long, help: "Skip components not installed on remote server")
    var skipMissing: Bool = false
    
    @Flag(name: .long, help: "Show what would be deployed without executing")
    var dryRun: Bool = false
    
    @Flag(name: .long, help: "Output as markdown instead of JSON")
    var markdown: Bool = false
    
    func run() throws {
        // Load project configuration
        guard let projectConfig = ConfigurationManager.readProject(id: projectId) else {
            outputError("Project '\(projectId)' not found")
            throw ExitCode.failure
        }
        
        // Validate server is configured
        guard let serverId = projectConfig.serverId,
              let serverConfig = ConfigurationManager.readServer(id: serverId),
              serverConfig.isValid else {
            outputError("Server not configured for project '\(projectId)'")
            throw ExitCode.failure
        }
        
        // Validate base path is configured
        guard let basePath = projectConfig.basePath, !basePath.isEmpty else {
            outputError("Remote base path not configured for project '\(projectId)'")
            throw ExitCode.failure
        }
        
        // Ensure SSH key exists for this server
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            outputError("SSH key not found for server. Configure SSH in Homeboy.app first.")
            throw ExitCode.failure
        }
        
        // Get all configured components
        let allComponents = projectConfig.components.map { DeployableComponent(from: $0) }
        
        guard !allComponents.isEmpty else {
            outputError("No components configured for project '\(projectId)'")
            throw ExitCode.failure
        }
        
        // Determine which components to deploy
        var componentsToDeploy: [DeployableComponent]
        
        if all {
            componentsToDeploy = allComponents
        } else if !componentIds.isEmpty {
            // Filter to requested components
            componentsToDeploy = []
            for id in componentIds {
                if let component = allComponents.first(where: { $0.id == id }) {
                    componentsToDeploy.append(component)
                } else {
                    outputError("Component '\(id)' not found in project config")
                    throw ExitCode.failure
                }
            }
        } else if outdated {
            // Will be filtered below after fetching remote versions
            componentsToDeploy = allComponents
        } else {
            outputError("No components specified. Use component IDs, --all, or --outdated")
            throw ExitCode.failure
        }
        
        // Fetch versions if needed for filtering
        var localVersions: [String: String] = [:]
        var remoteVersions: [String: String] = [:]
        
        if outdated || skipMissing {
            // Get local versions
            for component in componentsToDeploy {
                if let version = VersionParser.parseLocalVersion(for: component) {
                    localVersions[component.id] = version
                }
            }
            
            // Get remote versions
            remoteVersions = fetchRemoteVersions(
                components: componentsToDeploy,
                serverConfig: serverConfig,
                basePath: basePath
            )
            
            // Filter based on flags
            if outdated {
                componentsToDeploy = componentsToDeploy.filter { component in
                    let local = localVersions[component.id]
                    let remote = remoteVersions[component.id]
                    // Deploy if versions differ or remote doesn't exist
                    return local != remote
                }
            }
            
            if skipMissing {
                componentsToDeploy = componentsToDeploy.filter { component in
                    // Keep only components that exist on remote
                    remoteVersions[component.id] != nil
                }
            }
        } else {
            // Still get local versions for reporting
            for component in componentsToDeploy {
                if let version = VersionParser.parseLocalVersion(for: component) {
                    localVersions[component.id] = version
                }
            }
        }
        
        guard !componentsToDeploy.isEmpty else {
            outputResult(DeploymentResult(
                success: true,
                components: [],
                summary: DeploymentSummary(succeeded: 0, failed: 0, skipped: 0),
                message: "No components to deploy (all up to date or filtered out)"
            ))
            return
        }
        
        // Dry run - just show what would be deployed
        if dryRun {
            let dryRunComponents = componentsToDeploy.map { component in
                ComponentResult(
                    id: component.id,
                    name: component.name,
                    status: "would_deploy",
                    duration: nil,
                    localVersion: localVersions[component.id],
                    remoteVersion: remoteVersions[component.id],
                    error: nil
                )
            }
            outputResult(DeploymentResult(
                success: true,
                components: dryRunComponents,
                summary: DeploymentSummary(
                    succeeded: componentsToDeploy.count,
                    failed: 0,
                    skipped: 0
                ),
                message: "Dry run - no changes made"
            ))
            return
        }
        
        // Initialize deployment service
        guard let deploymentService = DeploymentService(project: projectConfig) else {
            outputError("Failed to initialize deployment service")
            throw ExitCode.failure
        }

        // Execute deployment
        var results: [ComponentResult] = []
        var succeeded = 0
        var failed = 0

        for component in componentsToDeploy {
            let startTime = Date()

            // Deploy-only pattern: check artifact exists first
            guard FileManager.default.fileExists(atPath: component.buildArtifactPath) else {
                results.append(ComponentResult(
                    id: component.id,
                    name: component.name,
                    status: "failed",
                    duration: nil,
                    localVersion: localVersions[component.id],
                    remoteVersion: remoteVersions[component.id],
                    error: "Build artifact not found. Run build first: cd \(component.localPath) && ./build.sh"
                ))
                failed += 1
                continue
            }

            // Use DeploymentService via semaphore for sync execution
            let semaphore = DispatchSemaphore(value: 0)
            var deployError: Error?

            deploymentService.deploy(component: component, onOutput: { line in
                fputs(line, stdout)
                fflush(stdout)
            }) { result in
                switch result {
                case .success:
                    break
                case .failure(let error):
                    deployError = error
                }
                semaphore.signal()
            }

            semaphore.wait()

            let duration = Date().timeIntervalSince(startTime)

            if let error = deployError {
                results.append(ComponentResult(
                    id: component.id,
                    name: component.name,
                    status: "failed",
                    duration: duration,
                    localVersion: localVersions[component.id],
                    remoteVersion: remoteVersions[component.id],
                    error: error.localizedDescription
                ))
                failed += 1
            } else {
                results.append(ComponentResult(
                    id: component.id,
                    name: component.name,
                    status: "deployed",
                    duration: duration,
                    localVersion: localVersions[component.id],
                    remoteVersion: remoteVersions[component.id],
                    error: nil
                ))
                succeeded += 1
            }
        }
        
        outputResult(DeploymentResult(
            success: failed == 0,
            components: results,
            summary: DeploymentSummary(
                succeeded: succeeded,
                failed: failed,
                skipped: 0
            ),
            message: nil
        ))
        
        if failed > 0 {
            throw ExitCode.failure
        }
    }
    
    // MARK: - Version Fetching

    private func fetchRemoteVersions(
        components: [DeployableComponent],
        serverConfig: ServerConfig,
        basePath: String
    ) -> [String: String] {
        var versions: [String: String] = [:]
        let resolver = RemotePathResolver(basePath: basePath)

        for component in components {
            guard let versionFilePath = resolver.versionFilePath(for: component) else { continue }

            // Fetch file content via SSH
            let result = executeSSHCommand(
                host: serverConfig.host,
                user: serverConfig.user,
                serverId: serverConfig.id,
                command: "cat '\(versionFilePath)' 2>/dev/null"
            )

            if result.success, !result.output.isEmpty {
                // Use VersionParser with component's custom pattern
                if let version = VersionParser.parseVersion(from: result.output, pattern: component.versionPattern) {
                    versions[component.id] = version
                }
            }
        }

        return versions
    }
    
    // MARK: - Output
    
    private func outputError(_ message: String) {
        if markdown {
            fputs("## Error\n\n\(message)\n", stderr)
        } else {
            let error = ["error": message]
            if let data = try? JSONSerialization.data(withJSONObject: error, options: [.prettyPrinted]),
               let json = String(data: data, encoding: .utf8) {
                fputs(json + "\n", stderr)
            } else {
                fputs("{\"error\": \"\(message)\"}\n", stderr)
            }
        }
    }
    
    private func outputResult(_ result: DeploymentResult) {
        if markdown {
            print(formatMarkdown(result))
        } else {
            print(formatJSON(result))
        }
    }
    
    private func formatJSON(_ result: DeploymentResult) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        
        guard let data = try? encoder.encode(result),
              let json = String(data: data, encoding: .utf8) else {
            return "{\"error\": \"Failed to encode result\"}"
        }
        
        return json
    }
    
    private func formatMarkdown(_ result: DeploymentResult) -> String {
        var output = "## Deployment Report\n\n"
        
        if let message = result.message {
            output += "\(message)\n\n"
        }
        
        for component in result.components {
            output += "### \(component.name)\n"
            output += "- **Status:** \(component.status.capitalized)\n"
            if let duration = component.duration {
                output += "- **Duration:** \(String(format: "%.1f", duration))s\n"
            }
            if let local = component.localVersion, let remote = component.remoteVersion {
                output += "- **Version:** \(remote) → \(local)\n"
            } else if let local = component.localVersion {
                output += "- **Version:** \(local)\n"
            }
            if let error = component.error {
                output += "- **Error:** \(error)\n"
            }
            output += "\n"
        }
        
        output += "### Summary\n"
        output += "| Succeeded | Failed | Skipped |\n"
        output += "|-----------|--------|--------|\n"
        output += "| \(result.summary.succeeded) | \(result.summary.failed) | \(result.summary.skipped) |\n"
        
        return output
    }
}

// MARK: - Result Types

struct DeploymentResult: Codable {
    let success: Bool
    let components: [ComponentResult]
    let summary: DeploymentSummary
    let message: String?
}

struct ComponentResult: Codable {
    let id: String
    let name: String
    let status: String
    let duration: Double?
    let localVersion: String?
    let remoteVersion: String?
    let error: String?
}

struct DeploymentSummary: Codable {
    let succeeded: Int
    let failed: Int
    let skipped: Int
}
