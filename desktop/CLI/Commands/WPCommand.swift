import ArgumentParser
import Foundation

/// WP-CLI passthrough command: homeboy wp <project> [blog-nickname] <args...>
struct WP: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "wp",
        abstract: "Execute WP-CLI commands on production"
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by WP-CLI command")
    var args: [String] = []
    
    func run() throws {
        guard !args.isEmpty else {
            fputs("Error: No WP-CLI command provided\n", stderr)
            fputs("Usage: homeboy wp <project> [blog-nickname] <command>\n", stderr)
            fputs("Example: homeboy wp extrachill plugin list\n", stderr)
            fputs("Example: homeboy wp extrachill shop plugin list\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project configuration
        guard let projectConfig = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
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
        
        // Validate WordPress config
        guard projectConfig.isWordPress,
              let wordpress = projectConfig.wordpress,
              wordpress.isConfigured else {
            fputs("Error: WordPress deployment not configured for project '\(projectId)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Ensure SSH key exists for this server
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
            throw ExitCode.failure
        }
        
        // Parse args: check if first arg is a blog nickname
        var wpArgs = args
        let (targetDomain, wasNickname) = resolveBlogDomain(
            projectConfig: projectConfig,
            potentialNickname: wpArgs.first ?? ""
        )
        
        if wasNickname {
            wpArgs.removeFirst()
        }
        
        guard !wpArgs.isEmpty else {
            fputs("Error: No WP-CLI command provided after blog nickname '\(args[0])'\n", stderr)
            throw ExitCode.failure
        }
        
        // Build the app path (wp-content parent)
        let appPath = extractAppPath(from: wordpress.wpContentPath)
        
        // Construct the remote WP-CLI command (cd ensures relative paths in wp-config.php work)
        let wpCommand = wpArgs.joined(separator: " ")
        let remoteCommand = "cd \(appPath) && wp \(wpCommand) --url=\(targetDomain)"
        
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
    
    private func extractAppPath(from wpContentPath: String) -> String {
        if wpContentPath.hasSuffix("/wp-content") {
            return String(wpContentPath.dropLast("/wp-content".count))
        }
        return wpContentPath
    }
}

// MARK: - Helper Functions

/// Resolves a potential blog nickname to a domain
func resolveBlogDomain(
    projectConfig: ProjectConfiguration,
    potentialNickname: String
) -> (domain: String, wasNickname: Bool) {
    guard let multisite = projectConfig.multisite,
          multisite.enabled else {
        return (projectConfig.domain, false)
    }
    
    // Case-insensitive match against blog names
    if let blog = multisite.blogs.first(where: {
        $0.name.lowercased() == potentialNickname.lowercased()
    }) {
        return (blog.domain, true)
    }
    
    return (projectConfig.domain, false)
}

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
