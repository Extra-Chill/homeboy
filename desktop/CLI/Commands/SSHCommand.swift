import ArgumentParser
import Foundation

/// SSH command: homeboy ssh <project> [command]
/// If command is provided, executes it and returns. Otherwise opens interactive shell.
struct SSH: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "ssh",
        abstract: "Execute SSH command on project server or open interactive shell"
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(help: "Command to execute (omit for interactive shell)")
    var command: String?
    
    func run() throws {
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
        
        // Ensure SSH key exists for this server
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
            throw ExitCode.failure
        }
        
        let keyPath = SSHService.keyPath(forServer: serverId)
        
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
        
        if let command = command {
            // Non-interactive: execute command and return
            process.arguments = [
                "-i", keyPath,
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "\(serverConfig.user)@\(serverConfig.host)",
                command
            ]
            
            // Stream output directly
            process.standardOutput = FileHandle.standardOutput
            process.standardError = FileHandle.standardError
        } else {
            // Interactive: open shell
            process.arguments = [
                "-i", keyPath,
                "-o", "StrictHostKeyChecking=no",
                "\(serverConfig.user)@\(serverConfig.host)"
            ]
            
            // Connect stdin/stdout/stderr for interactive use
            process.standardInput = FileHandle.standardInput
            process.standardOutput = FileHandle.standardOutput
            process.standardError = FileHandle.standardError
        }
        
        try process.run()
        process.waitUntilExit()
        
        if process.terminationStatus != 0 {
            throw ExitCode(process.terminationStatus)
        }
    }
}
