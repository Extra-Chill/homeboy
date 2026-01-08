import ArgumentParser
import Foundation

// MARK: - Tool-Specific Commands

/// PM2 remote command: homeboy pm2 <project> [sub-target] <args...>
struct PM2: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "pm2",
        abstract: "Execute PM2 commands on remote Node.js servers"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "PM2 command and arguments")
    var args: [String] = []
    
    func run() throws {
        try RemoteCLI.execute(tool: "pm2", projectId: projectId, args: args)
    }
}

/// WP-CLI remote command: homeboy wp <project> [sub-target] <args...>
struct WP: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "wp",
        abstract: "Execute WP-CLI commands on remote WordPress servers"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by WP-CLI command")
    var args: [String] = []
    
    func run() throws {
        try RemoteCLI.execute(tool: "wp", projectId: projectId, args: args)
    }
}

// MARK: - Shared Remote CLI Logic

/// Shared implementation for remote CLI commands.
/// Each tool-specific command delegates to this.
enum RemoteCLI {
    
    static func execute(tool: String, projectId: String, args: [String]) throws {
        guard !args.isEmpty else {
            fputs("Error: No command provided\n", stderr)
            fputs("Usage: homeboy \(tool) <project> [sub-target] <command...>\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project configuration
        guard let projectConfig = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project type definition to get CLI config
        guard let typeDefinition = loadProjectTypeDefinition(id: projectConfig.projectType) else {
            fputs("Error: Unknown project type '\(projectConfig.projectType)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate CLI tool matches project type
        guard let cliConfig = typeDefinition.cli else {
            fputs("Error: Project type '\(typeDefinition.displayName)' does not support remote CLI\n", stderr)
            throw ExitCode.failure
        }
        
        guard cliConfig.tool == tool else {
            fputs("Error: Project '\(projectId)' is a \(typeDefinition.displayName) project (uses '\(cliConfig.tool)', not '\(tool)')\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate server is configured
        guard let serverId = projectConfig.serverId,
              let serverConfig = ConfigurationManager.readServer(id: serverId),
              !serverConfig.host.isEmpty,
              !serverConfig.user.isEmpty else {
            fputs("Error: Server not configured for project '\(projectId)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Ensure SSH key exists for this server
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
            throw ExitCode.failure
        }
        
        // Parse args: check if first arg is a sub-target
        var commandArgs = args
        let (targetDomain, wasSubTarget) = resolveSubTarget(
            projectConfig: projectConfig,
            potentialSubTarget: commandArgs.first ?? ""
        )
        
        if wasSubTarget {
            commandArgs.removeFirst()
        }
        
        guard !commandArgs.isEmpty else {
            fputs("Error: No command provided after sub-target '\(args[0])'\n", stderr)
            throw ExitCode.failure
        }
        
        // Build template variables
        let variables = buildTemplateVariables(
            projectConfig: projectConfig,
            targetDomain: targetDomain,
            args: commandArgs
        )
        
        // Render command from template
        let remoteCommand = TemplateRenderer.render(cliConfig.commandTemplate, variables: variables)
        
        // Execute via SSH
        let result = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverId,
            command: remoteCommand
        )
        
        // Output result
        print(result.output, terminator: "")
        
        if !result.success {
            throw ExitCode.failure
        }
    }
    
    /// Resolves a potential sub-target ID to a domain
    private static func resolveSubTarget(
        projectConfig: ProjectConfiguration,
        potentialSubTarget: String
    ) -> (domain: String, wasSubTarget: Bool) {
        let subTargets = projectConfig.subTargets
        guard !subTargets.isEmpty else {
            return (projectConfig.domain, false)
        }
        
        // Case-insensitive match against sub-target IDs
        if let subTarget = subTargets.first(where: {
            $0.id.lowercased() == potentialSubTarget.lowercased()
        }) {
            return (subTarget.domain, true)
        }
        
        return (projectConfig.domain, false)
    }
    
    /// Builds the template variable dictionary for command rendering
    private static func buildTemplateVariables(
        projectConfig: ProjectConfiguration,
        targetDomain: String,
        args: [String]
    ) -> [String: String] {
        var variables: [String: String] = [
            "projectId": projectConfig.id,
            "domain": projectConfig.domain,
            "targetDomain": targetDomain,
            "args": args.joined(separator: " ")
        ]
        
        // Add basePath if configured
        if let basePath = projectConfig.basePath {
            variables["basePath"] = basePath
            
            // appPath: for WordPress, parent of wp-content; otherwise same as basePath
            if projectConfig.isWordPress,
               let wordpress = projectConfig.wordpress,
               wordpress.isConfigured {
                variables["appPath"] = extractAppPath(from: wordpress.wpContentPath)
            } else {
                variables["appPath"] = basePath
            }
        }
        
        return variables
    }
    
    /// Extracts the app path (parent of wp-content for WordPress)
    private static func extractAppPath(from wpContentPath: String) -> String {
        if wpContentPath.hasSuffix("/wp-content") {
            return String(wpContentPath.dropLast("/wp-content".count))
        }
        return wpContentPath
    }
}

// MARK: - Project Configuration Loading

/// Loads project configuration from disk
func loadProjectConfig(id: String) -> ProjectConfiguration? {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let projectPath = appSupport.appendingPathComponent("Homeboy/projects/\(id).json")
    
    guard let data = try? Data(contentsOf: projectPath),
          let config = try? JSONDecoder().decode(ProjectConfiguration.self, from: data) else {
        return nil
    }
    
    return config
}

// MARK: - Project Type Loading

/// Loads a project type definition from bundled or user resources.
/// Works in CLI context where Bundle.main may not have the resources.
func loadProjectTypeDefinition(id: String) -> ProjectTypeDefinition? {
    let fileManager = FileManager.default
    let decoder = JSONDecoder()
    
    // Try user-defined types first
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let userTypePath = appSupport
        .appendingPathComponent("Homeboy/project-types/\(id).json")
    
    if let data = try? Data(contentsOf: userTypePath),
       let type = try? decoder.decode(ProjectTypeDefinition.self, from: data) {
        return type
    }
    
    // Try bundled types (in the app bundle)
    // The CLI binary is at: Homeboy.app/Contents/MacOS/homeboy-cli
    // Resources are at: Homeboy.app/Contents/Resources/project-types/
    if let bundleURL = Bundle.main.bundleURL.deletingLastPathComponent()
        .deletingLastPathComponent()
        .appendingPathComponent("Resources/project-types/\(id).json") as URL?,
       let data = try? Data(contentsOf: bundleURL),
       let type = try? decoder.decode(ProjectTypeDefinition.self, from: data) {
        return type
    }
    
    // Fallback: try current bundle's resources (for development)
    if let bundledURL = Bundle.main.url(forResource: id, withExtension: "json", subdirectory: "project-types"),
       let data = try? Data(contentsOf: bundledURL),
       let type = try? decoder.decode(ProjectTypeDefinition.self, from: data) {
        return type
    }
    
    return nil
}

// MARK: - SSH Command Execution

/// Executes an SSH command and returns the result
func executeSSHCommand(host: String, user: String, serverId: String, command: String) -> (success: Bool, output: String) {
    let keyPath = SSHService.keyPath(forServer: serverId)
    
    // Ensure key file exists
    guard SSHService.ensureKeyFileExists(forServer: serverId) else {
        return (false, "SSH key not found for server \(serverId)")
    }
    
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
    process.arguments = [
        "-i", keyPath,
        "-o", "StrictHostKeyChecking=no",
        "-o", "BatchMode=yes",
        "\(user)@\(host)",
        command
    ]
    
    let outputPipe = Pipe()
    let errorPipe = Pipe()
    process.standardOutput = outputPipe
    process.standardError = errorPipe
    
    do {
        try process.run()
        process.waitUntilExit()
        
        let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
        let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
        
        let output = String(data: outputData, encoding: .utf8) ?? ""
        let errorOutput = String(data: errorData, encoding: .utf8) ?? ""
        
        let combinedOutput = output + errorOutput
        
        return (process.terminationStatus == 0, combinedOutput)
    } catch {
        return (false, "Error: \(error.localizedDescription)\n")
    }
}
