import ArgumentParser
import Foundation

// MARK: - Server Command

struct Server: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "server",
        abstract: "Configure SSH server connections",
        discussion: """
            Create and configure SSH server connections for deployments.

            Examples:
              homeboy server create "Production" --host server.example.com --user deploy
              homeboy server set production-1 --port 2222
              homeboy server list

            Note: SSH keys must be configured in Homeboy.app after server creation.

            See 'homeboy docs server' for full documentation.
            """,
        subcommands: [
            ServerCreate.self,
            ServerShow.self,
            ServerSet.self,
            ServerDelete.self,
            ServerList.self
        ]
    )
}

// MARK: - Server Create

struct ServerCreate: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "create",
        abstract: "Register a new SSH server for deployments",
        discussion: """
            Registers a new SSH server. The server ID is auto-generated from hostname.

            Example:
              homeboy server create "Production" --host server.example.com --user deploy

            After creation, configure SSH key in Homeboy.app Settings > Servers.
            """
    )
    
    @Argument(help: "Server name")
    var name: String
    
    @Option(name: .long, help: "SSH host")
    var host: String
    
    @Option(name: .long, help: "SSH username")
    var user: String
    
    @Option(name: .long, help: "SSH port (default: 22)")
    var port: Int = 22
    
    func run() throws {
        let id = ServerConfig.generateId(from: host)
        
        // Check if server already exists
        if ConfigurationManager.readServer(id: id) != nil {
            fputs("Error: Server '\(id)' already exists\n", stderr)
            throw ExitCode.failure
        }
        
        let server = ServerConfig(
            id: id,
            name: name,
            host: host,
            user: user,
            port: port
        )
        
        try saveServerConfig(server)
        
        let result: [String: Any] = [
            "success": true,
            "id": id,
            "name": name,
            "host": host,
            "user": user,
            "port": port,
            "note": "SSH key must be configured in Homeboy.app"
        ]
        print(formatJSON(result))
    }
}

// MARK: - Server Show

struct ServerShow: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "show",
        abstract: "Display server configuration as JSON"
    )
    
    @Argument(help: "Server ID")
    var serverId: String
    
    func run() throws {
        guard let server = ConfigurationManager.readServer(id: serverId) else {
            fputs("Error: Server '\(serverId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(server)
        print(String(data: data, encoding: .utf8) ?? "{}")
    }
}

// MARK: - Server Set

struct ServerSet: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "set",
        abstract: "Modify server connection settings",
        discussion: """
            Modifies server connection settings.

            Example:
              homeboy server set production-1 --port 2222
              homeboy server set production-1 --user newadmin
            """
    )
    
    @Argument(help: "Server ID")
    var serverId: String
    
    @Option(name: .long, help: "Server display name")
    var name: String?
    
    @Option(name: .long, help: "SSH host")
    var host: String?
    
    @Option(name: .long, help: "SSH username")
    var user: String?
    
    @Option(name: .long, help: "SSH port")
    var port: Int?
    
    func run() throws {
        guard var server = ConfigurationManager.readServer(id: serverId) else {
            fputs("Error: Server '\(serverId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        var changes: [String] = []
        
        if let name = name {
            server.name = name
            changes.append("name")
        }
        
        if let host = host {
            server.host = host
            changes.append("host")
        }
        
        if let user = user {
            server.user = user
            changes.append("user")
        }
        
        if let port = port {
            server.port = port
            changes.append("port")
        }
        
        guard !changes.isEmpty else {
            fputs("Error: No changes specified\n", stderr)
            throw ExitCode.failure
        }
        
        try saveServerConfig(server)
        
        let result: [String: Any] = [
            "success": true,
            "id": serverId,
            "updated": changes
        ]
        print(formatJSON(result))
    }
}

// MARK: - Server Delete

struct ServerDelete: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "delete",
        abstract: "Remove a server configuration",
        discussion: """
            Removes a server configuration. Requires --force flag.

            Example:
              homeboy server delete old-server --force

            Note: Cannot delete servers that are linked to projects.
            """
    )
    
    @Argument(help: "Server ID")
    var serverId: String
    
    @Flag(name: .long, help: "Confirm deletion")
    var force: Bool = false
    
    func run() throws {
        guard force else {
            fputs("Error: Use --force to confirm deletion\n", stderr)
            throw ExitCode.failure
        }
        
        guard ConfigurationManager.readServer(id: serverId) != nil else {
            fputs("Error: Server '\(serverId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Check if any project uses this server
        let projectIds = getAvailableProjectIds()
        for projectId in projectIds {
            if let project = ConfigurationManager.readProject(id: projectId), project.serverId == serverId {
                fputs("Error: Server is used by project '\(projectId)'. Update or delete the project first.\n", stderr)
                throw ExitCode.failure
            }
        }
        
        try deleteServerConfig(id: serverId)
        
        let result: [String: Any] = [
            "success": true,
            "deleted": serverId
        ]
        print(formatJSON(result))
    }
}

// MARK: - Server List

struct ServerList: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "list",
        abstract: "Show all configured SSH servers"
    )
    
    func run() throws {
        let serverIds = getAvailableServerIds()
        var servers: [[String: Any]] = []
        
        for id in serverIds {
            if let server = ConfigurationManager.readServer(id: id) {
                servers.append([
                    "id": server.id,
                    "name": server.name,
                    "host": server.host,
                    "user": server.user,
                    "port": server.port
                ])
            }
        }
        
        print(formatJSON(["servers": servers]))
    }
}

// MARK: - Helper Functions

/// Get available project IDs
private func getAvailableProjectIds() -> [String] {
    guard let files = try? FileManager.default.contentsOfDirectory(at: AppPaths.projects, includingPropertiesForKeys: nil) else {
        return []
    }

    return files
        .filter { $0.pathExtension == "json" }
        .map { $0.deletingPathExtension().lastPathComponent }
        .sorted()
}

/// Get available server IDs
private func getAvailableServerIds() -> [String] {
    guard let files = try? FileManager.default.contentsOfDirectory(at: AppPaths.servers, includingPropertiesForKeys: nil) else {
        return []
    }

    return files
        .filter { $0.pathExtension == "json" }
        .map { $0.deletingPathExtension().lastPathComponent }
        .sorted()
}

/// Save server configuration to disk
private func saveServerConfig(_ server: ServerConfig) throws {
    // Ensure directory exists
    try FileManager.default.createDirectory(at: AppPaths.servers, withIntermediateDirectories: true)

    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let data = try encoder.encode(server)
    try data.write(to: AppPaths.server(id: server.id))
}

/// Delete server configuration from disk
private func deleteServerConfig(id: String) throws {
    try FileManager.default.removeItem(at: AppPaths.server(id: id))
}

/// Format dictionary as JSON string
private func formatJSON(_ dict: [String: Any]) -> String {
    guard let data = try? JSONSerialization.data(withJSONObject: dict, options: [.prettyPrinted, .sortedKeys]),
          let json = String(data: data, encoding: .utf8) else {
        return "{}"
    }
    return json
}
