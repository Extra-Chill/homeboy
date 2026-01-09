import ArgumentParser
import Foundation

// MARK: - Server Command

/// Server management: homeboy server <subcommand>
struct Server: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "server",
        abstract: "Manage server configurations",
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

/// Create a new server: homeboy server create <name> --host <host> --user <user>
struct ServerCreate: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "create",
        abstract: "Create a new server configuration"
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

/// Show server configuration: homeboy server show <id>
struct ServerShow: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "show",
        abstract: "Show server configuration"
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

/// Update server fields: homeboy server set <id> <--flag value>...
struct ServerSet: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "set",
        abstract: "Update server configuration fields"
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

/// Delete a server: homeboy server delete <id> --force
struct ServerDelete: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "delete",
        abstract: "Delete a server configuration"
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
            if let project = loadProjectConfig(id: projectId), project.serverId == serverId {
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

/// List all servers: homeboy server list
struct ServerList: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "list",
        abstract: "List all server configurations"
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
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let projectsDir = appSupport.appendingPathComponent("Homeboy/projects")
    
    guard let files = try? fileManager.contentsOfDirectory(at: projectsDir, includingPropertiesForKeys: nil) else {
        return []
    }
    
    return files
        .filter { $0.pathExtension == "json" }
        .map { $0.deletingPathExtension().lastPathComponent }
        .sorted()
}

/// Get available server IDs
private func getAvailableServerIds() -> [String] {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let serversDir = appSupport.appendingPathComponent("Homeboy/servers")
    
    guard let files = try? fileManager.contentsOfDirectory(at: serversDir, includingPropertiesForKeys: nil) else {
        return []
    }
    
    return files
        .filter { $0.pathExtension == "json" }
        .map { $0.deletingPathExtension().lastPathComponent }
        .sorted()
}

/// Save server configuration to disk
private func saveServerConfig(_ server: ServerConfig) throws {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let serversDir = appSupport.appendingPathComponent("Homeboy/servers")
    
    // Ensure directory exists
    try fileManager.createDirectory(at: serversDir, withIntermediateDirectories: true)
    
    let serverPath = serversDir.appendingPathComponent("\(server.id).json")
    
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let data = try encoder.encode(server)
    try data.write(to: serverPath)
}

/// Delete server configuration from disk
private func deleteServerConfig(id: String) throws {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let serverPath = appSupport.appendingPathComponent("Homeboy/servers/\(id).json")
    try fileManager.removeItem(at: serverPath)
}

/// Format dictionary as JSON string
private func formatJSON(_ dict: [String: Any]) -> String {
    guard let data = try? JSONSerialization.data(withJSONObject: dict, options: [.prettyPrinted, .sortedKeys]),
          let json = String(data: data, encoding: .utf8) else {
        return "{}"
    }
    return json
}
