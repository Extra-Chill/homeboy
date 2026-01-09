import ArgumentParser
import Foundation

// MARK: - Tool-Specific Commands

struct PM2: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "pm2",
        abstract: "Run PM2 commands on production Node.js servers",
        discussion: """
            Runs PM2 commands on the project's remote server.

            Usage:
              homeboy pm2 <project> [subtarget] <command...>

            Examples:
              homeboy pm2 api-server list
              homeboy pm2 api-server restart app
              homeboy pm2 api-server logs --lines 100

            Prerequisites: Server and base path configured.

            See 'homeboy docs pm2' for full documentation.
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Flag(name: .long, help: "Execute locally instead of on remote server")
    var local: Bool = false
    
    @Argument(parsing: .captureForPassthrough, help: "PM2 command and arguments")
    var args: [String] = []
    
    func run() throws {
        try CLIExecutor.execute(tool: "pm2", projectId: projectId, args: args, local: local)
    }
}

struct WP: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "wp",
        abstract: "Run WP-CLI commands on production WordPress sites",
        discussion: """
            Runs WP-CLI commands on the project's remote server.

            Usage:
              homeboy wp <project> [subtarget] <command...>
              homeboy wp <project> --local [subtarget] <command...>

            Examples:
              homeboy wp extrachill plugin list
              homeboy wp extrachill shop plugin list    # Multisite subtarget
              homeboy wp extrachill --local plugin list # Local execution

            Prerequisites:
              Remote: Server and base path configured
              Local: localCLI.sitePath configured in project

            See 'homeboy docs wp' for full documentation.
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Flag(name: .long, help: "Execute locally instead of on remote server")
    var local: Bool = false
    
    @Argument(parsing: .captureForPassthrough, help: "Optional sub-target followed by WP-CLI command")
    var args: [String] = []
    
    func run() throws {
        try CLIExecutor.execute(tool: "wp", projectId: projectId, args: args, local: local)
    }
}

// MARK: - CLI Executor

/// Unified CLI execution for both local and remote contexts.
/// Uses project type templates with context-specific variables.
enum CLIExecutor {
    
    static func execute(tool: String, projectId: String, args: [String], local: Bool) throws {
        guard !args.isEmpty else {
            fputs("Error: No command provided\n", stderr)
            fputs("Usage: homeboy \(tool) <project> [--local] [sub-target] <command...>\n", stderr)
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
            fputs("Error: Project type '\(typeDefinition.displayName)' does not support CLI\n", stderr)
            throw ExitCode.failure
        }
        
        guard cliConfig.tool == tool else {
            fputs("Error: Project '\(projectId)' is a \(typeDefinition.displayName) project (uses '\(cliConfig.tool)', not '\(tool)')\n", stderr)
            throw ExitCode.failure
        }
        
        if local {
            try executeLocal(
                projectConfig: projectConfig,
                cliConfig: cliConfig,
                args: args
            )
        } else {
            try executeRemote(
                projectConfig: projectConfig,
                cliConfig: cliConfig,
                args: args
            )
        }
    }
    
    // MARK: - Remote Execution
    
    private static func executeRemote(
        projectConfig: ProjectConfiguration,
        cliConfig: CLIConfig,
        args: [String]
    ) throws {
        // Validate server is configured
        guard let serverId = projectConfig.serverId,
              let serverConfig = ConfigurationManager.readServer(id: serverId),
              !serverConfig.host.isEmpty,
              !serverConfig.user.isEmpty else {
            fputs("Error: Server not configured for project '\(projectConfig.id)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Ensure SSH key exists for this server
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate basePath is configured
        guard let basePath = projectConfig.basePath, !basePath.isEmpty else {
            fputs("Error: Remote base path not configured for project '\(projectConfig.id)'\n", stderr)
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
        
        // Build template variables for remote context
        let variables = buildRemoteVariables(
            projectConfig: projectConfig,
            cliConfig: cliConfig,
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
    
    // MARK: - Local Execution
    
    private static func executeLocal(
        projectConfig: ProjectConfiguration,
        cliConfig: CLIConfig,
        args: [String]
    ) throws {
        // Validate local CLI is configured
        guard projectConfig.localCLI.isConfigured else {
            fputs("Error: Local CLI not configured for project '\(projectConfig.id)'\n", stderr)
            fputs("Configure 'Local Site Path' in Homeboy.app Settings.\n", stderr)
            throw ExitCode.failure
        }
        
        // Parse args: check if first arg is a sub-target
        var commandArgs = args
        let (targetDomain, wasSubTarget) = resolveSubTarget(
            projectConfig: projectConfig,
            potentialSubTarget: commandArgs.first ?? "",
            useLocalDomain: true
        )
        
        if wasSubTarget {
            commandArgs.removeFirst()
        }
        
        guard !commandArgs.isEmpty else {
            fputs("Error: No command provided after sub-target '\(args[0])'\n", stderr)
            throw ExitCode.failure
        }
        
        // Build template variables for local context
        let variables = buildLocalVariables(
            projectConfig: projectConfig,
            cliConfig: cliConfig,
            targetDomain: targetDomain,
            args: commandArgs
        )
        
        // Render command from template
        let localCommand = TemplateRenderer.render(cliConfig.commandTemplate, variables: variables)
        
        // Execute locally
        let result = executeLocalCommand(localCommand)
        
        // Output result
        print(result.output, terminator: "")
        
        if !result.success {
            throw ExitCode.failure
        }
    }
    
    // MARK: - Sub-Target Resolution
    
    /// Resolves a potential sub-target ID to a domain
    private static func resolveSubTarget(
        projectConfig: ProjectConfiguration,
        potentialSubTarget: String,
        useLocalDomain: Bool = false
    ) -> (domain: String, wasSubTarget: Bool) {
        let defaultDomain = useLocalDomain 
            ? (projectConfig.localCLI.domain.isEmpty ? "localhost" : projectConfig.localCLI.domain)
            : projectConfig.domain
        
        let subTargets = projectConfig.subTargets
        guard !subTargets.isEmpty else {
            return (defaultDomain, false)
        }
        
        // Case-insensitive match against sub-target IDs or names
        if let subTarget = subTargets.first(where: {
            $0.id.lowercased() == potentialSubTarget.lowercased() ||
            $0.name.lowercased() == potentialSubTarget.lowercased()
        }) {
            // For local, derive domain from local domain + subtarget path
            if useLocalDomain {
                let baseDomain = projectConfig.localCLI.domain.isEmpty ? "localhost" : projectConfig.localCLI.domain
                let urlPath = subTarget.isDefault ? "" : "/\(subTarget.id)"
                return ("\(baseDomain)\(urlPath)", true)
            } else {
                return (subTarget.domain, true)
            }
        }
        
        return (defaultDomain, false)
    }
    
    // MARK: - Variable Building
    
    /// Builds template variables for remote execution
    private static func buildRemoteVariables(
        projectConfig: ProjectConfiguration,
        cliConfig: CLIConfig,
        targetDomain: String,
        args: [String]
    ) -> [String: String] {
        var variables: [String: String] = [
            TemplateRenderer.Variables.projectId: projectConfig.id,
            TemplateRenderer.Variables.domain: targetDomain,
            TemplateRenderer.Variables.args: args.joined(separator: " "),
            TemplateRenderer.Variables.cliPath: cliConfig.defaultCLIPath ?? cliConfig.tool
        ]
        
        // sitePath = basePath for remote execution
        if let basePath = projectConfig.basePath {
            variables[TemplateRenderer.Variables.sitePath] = basePath
            variables[TemplateRenderer.Variables.basePath] = basePath  // Legacy compat
        }
        
        return variables
    }
    
    /// Builds template variables for local execution
    private static func buildLocalVariables(
        projectConfig: ProjectConfiguration,
        cliConfig: CLIConfig,
        targetDomain: String,
        args: [String]
    ) -> [String: String] {
        let cliPath = projectConfig.localCLI.cliPath ?? cliConfig.defaultCLIPath ?? cliConfig.tool
        
        return [
            TemplateRenderer.Variables.projectId: projectConfig.id,
            TemplateRenderer.Variables.domain: targetDomain,
            TemplateRenderer.Variables.args: args.joined(separator: " "),
            TemplateRenderer.Variables.sitePath: projectConfig.localCLI.sitePath,
            TemplateRenderer.Variables.cliPath: cliPath
        ]
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

/// Loads a project type definition from Application Support.
/// Project types are synced to Application Support on app launch.
func loadProjectTypeDefinition(id: String) -> ProjectTypeDefinition? {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let typePath = appSupport.appendingPathComponent("Homeboy/project-types/\(id).json")
    
    guard let data = try? Data(contentsOf: typePath),
          let type = try? JSONDecoder().decode(ProjectTypeDefinition.self, from: data) else {
        return nil
    }
    
    return type
}

// MARK: - Command Execution

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

/// Executes a local shell command and returns the result
func executeLocalCommand(_ command: String) -> (success: Bool, output: String) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/bash")
    process.arguments = ["-c", command]
    
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
