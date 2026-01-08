import ArgumentParser
import Foundation

/// Deploy components to production: homeboy deploy <project> [component-id...] [flags]
struct Deploy: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "deploy",
        abstract: "Deploy plugins and themes to production"
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
        guard let projectConfig = loadProjectConfig(id: projectId) else {
            outputError("Project '\(projectId)' not found")
            throw ExitCode.failure
        }
        
        // Validate server is configured
        guard let serverId = projectConfig.serverId,
              let serverConfig = ConfigurationManager.readServer(id: serverId),
              !serverConfig.host.isEmpty,
              !serverConfig.user.isEmpty else {
            outputError("Server not configured for project '\(projectId)'")
            throw ExitCode.failure
        }
        
        // Validate WordPress config for WordPress projects
        guard projectConfig.projectType == .wordpress,
              let wordpress = projectConfig.wordpress,
              wordpress.isConfigured else {
            outputError("WordPress deployment not configured for project '\(projectId)'")
            throw ExitCode.failure
        }
        
        let wpContentPath = wordpress.wpContentPath
        
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
                wpContentPath: wpContentPath
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
        
        // Execute deployment
        var results: [ComponentResult] = []
        var succeeded = 0
        var failed = 0
        
        for component in componentsToDeploy {
            let startTime = Date()
            
            let result = deployComponent(
                component: component,
                serverConfig: serverConfig,
                wpContentPath: wpContentPath
            )
            
            let duration = Date().timeIntervalSince(startTime)
            
            results.append(ComponentResult(
                id: component.id,
                name: component.name,
                status: result.success ? "deployed" : "failed",
                duration: duration,
                localVersion: localVersions[component.id],
                remoteVersion: remoteVersions[component.id],
                error: result.error
            ))
            
            if result.success {
                succeeded += 1
            } else {
                failed += 1
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
    
    // MARK: - Deployment Logic
    
    private func deployComponent(
        component: DeployableComponent,
        serverConfig: ServerConfig,
        wpContentPath: String
    ) -> (success: Bool, error: String?) {
        
        // 1. Build locally
        let buildResult = executeBuild(at: component.localPath)
        guard buildResult.success else {
            return (false, "Build failed: \(buildResult.error ?? "Unknown error")")
        }
        
        // 2. Verify zip exists
        guard FileManager.default.fileExists(atPath: component.buildOutputPath) else {
            return (false, "Build completed but zip not found at \(component.buildOutputPath)")
        }
        
        // 3. Upload via SCP
        let uploadResult = executeSCPUpload(
            localPath: component.buildOutputPath,
            remotePath: "tmp/\(component.id).zip",
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverConfig.id
        )
        guard uploadResult.success else {
            return (false, "Upload failed: \(uploadResult.error ?? "Unknown error")")
        }
        
        // 4. Remove old version
        let remotePath = "\(wpContentPath)/\(component.remotePath)"
        let removeResult = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverConfig.id,
            command: "rm -rf \"\(remotePath)\""
        )
        guard removeResult.success else {
            return (false, "Failed to remove old version: \(removeResult.output)")
        }
        
        // 5. Extract
        let targetDir = component.type == .theme
            ? "\(wpContentPath)/themes"
            : "\(wpContentPath)/plugins"
        let extractResult = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverConfig.id,
            command: "unzip -o ~/tmp/\(component.id).zip -d \"\(targetDir)\" && chmod -R 755 \"\(targetDir)/\(component.id)\""
        )
        guard extractResult.success else {
            return (false, "Extraction failed: \(extractResult.output)")
        }
        
        // 6. Cleanup
        _ = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverConfig.id,
            command: "rm -f ~/tmp/\(component.id).zip"
        )
        
        return (true, nil)
    }
    
    private func executeBuild(at directoryPath: String) -> (success: Bool, error: String?) {
        let buildScriptPath = "\(directoryPath)/build.sh"
        
        guard FileManager.default.fileExists(atPath: buildScriptPath) else {
            return (false, "build.sh not found at \(buildScriptPath)")
        }
        
        let process = Process()
        process.currentDirectoryURL = URL(fileURLWithPath: directoryPath)
        process.executableURL = URL(fileURLWithPath: "/bin/bash")
        process.arguments = ["build.sh"]
        process.environment = [
            "PATH": "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": FileManager.default.homeDirectoryForCurrentUser.path
        ]
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        do {
            try process.run()
            process.waitUntilExit()
            
            if process.terminationStatus == 0 {
                return (true, nil)
            } else {
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8) ?? ""
                return (false, "Exit code \(process.terminationStatus): \(output)")
            }
        } catch {
            return (false, error.localizedDescription)
        }
    }
    
    private func fetchRemoteVersions(
        components: [DeployableComponent],
        serverConfig: ServerConfig,
        wpContentPath: String
    ) -> [String: String] {
        var versions: [String: String] = [:]
        
        // Build a command to grep versions from all components in one SSH call
        var versionChecks: [String] = []
        for component in components {
            let remotePath = "\(wpContentPath)/\(component.remotePath)/\(component.mainFile)"
            versionChecks.append("echo '\(component.id):'$(grep -m1 'Version:' \"\(remotePath)\" 2>/dev/null | sed 's/.*Version:[[:space:]]*//' | tr -d '[:space:]')")
        }
        
        let command = versionChecks.joined(separator: " && ")
        let result = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverConfig.id,
            command: command
        )
        
        if result.success {
            let lines = result.output.components(separatedBy: .newlines)
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

// MARK: - SCP Helper

func executeSCPUpload(localPath: String, remotePath: String, host: String, user: String, serverId: String) -> (success: Bool, error: String?) {
    let keyPath = SSHService.keyPath(forServer: serverId)
    
    // Ensure key file exists
    guard SSHService.ensureKeyFileExists(forServer: serverId) else {
        return (false, "SSH key not found for server \(serverId)")
    }
    
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/scp")
    process.arguments = [
        "-i", keyPath,
        "-o", "StrictHostKeyChecking=no",
        "-o", "BatchMode=yes",
        localPath,
        "\(user)@\(host):\(remotePath)"
    ]
    
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = pipe
    
    do {
        try process.run()
        process.waitUntilExit()
        
        if process.terminationStatus == 0 {
            return (true, nil)
        } else {
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8) ?? ""
            return (false, output)
        }
    } catch {
        return (false, error.localizedDescription)
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
