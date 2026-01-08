import ArgumentParser
import Foundation

/// Database commands: homeboy db <project> [blog-nickname] <subcommand>
struct DB: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "db",
        abstract: "Database operations (read-only)",
        subcommands: [DBTables.self, DBDescribe.self, DBQuery.self]
    )
}

/// List database tables: homeboy db <project> [blog-nickname] tables
struct DBTables: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "tables",
        abstract: "List database tables"
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        let (projectConfig, serverConfig, wordpress, serverId) = try validateWordPressProject(projectId: projectId)
        
        // Parse args for optional blog nickname
        let (targetDomain, _) = resolveBlogDomain(
            projectConfig: projectConfig,
            potentialNickname: args.first ?? ""
        )
        
        let appPath = extractAppPath(from: wordpress.wpContentPath)
        let format = json ? "json" : "table"
        let remoteCommand = "wp db query \"SHOW TABLE STATUS\" --format=\(format) --path=\(appPath) --url=\(targetDomain)"
        
        let result = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverId,
            command: remoteCommand
        )
        
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

/// Describe a table: homeboy db <project> [blog-nickname] describe <table>
struct DBDescribe: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "describe",
        abstract: "Show table structure"
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by table name")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        let (projectConfig, serverConfig, wordpress, serverId) = try validateWordPressProject(projectId: projectId)
        
        guard !args.isEmpty else {
            fputs("Error: Table name required\n", stderr)
            fputs("Usage: homeboy db <project> describe <table>\n", stderr)
            throw ExitCode.failure
        }
        
        // Parse args: check if first arg is a blog nickname
        var remainingArgs = args
        let (targetDomain, wasNickname) = resolveBlogDomain(
            projectConfig: projectConfig,
            potentialNickname: remainingArgs.first ?? ""
        )
        
        if wasNickname {
            remainingArgs.removeFirst()
        }
        
        guard let tableName = remainingArgs.first else {
            fputs("Error: Table name required after blog nickname\n", stderr)
            throw ExitCode.failure
        }
        
        let appPath = extractAppPath(from: wordpress.wpContentPath)
        let format = json ? "json" : "table"
        let remoteCommand = "wp db query \"DESCRIBE \(tableName)\" --format=\(format) --path=\(appPath) --url=\(targetDomain)"
        
        let result = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverId,
            command: remoteCommand
        )
        
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

/// Execute SQL query: homeboy db <project> [blog-nickname] query "<sql>"
struct DBQuery: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "query",
        abstract: "Execute a SQL query (read-only)"
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by SQL query")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        let (projectConfig, serverConfig, wordpress, serverId) = try validateWordPressProject(projectId: projectId)
        
        guard !args.isEmpty else {
            fputs("Error: SQL query required\n", stderr)
            fputs("Usage: homeboy db <project> query \"SELECT * FROM wp_users\"\n", stderr)
            throw ExitCode.failure
        }
        
        // Parse args: check if first arg is a blog nickname
        var remainingArgs = args
        let (targetDomain, wasNickname) = resolveBlogDomain(
            projectConfig: projectConfig,
            potentialNickname: remainingArgs.first ?? ""
        )
        
        if wasNickname {
            remainingArgs.removeFirst()
        }
        
        // Join remaining args as the SQL query
        let sql = remainingArgs.joined(separator: " ")
        
        guard !sql.isEmpty else {
            fputs("Error: SQL query required after blog nickname\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate read-only query
        try validateReadOnly(sql)
        
        // Escape the SQL for shell
        let escapedSQL = sql.replacingOccurrences(of: "\"", with: "\\\"")
        
        let appPath = extractAppPath(from: wordpress.wpContentPath)
        let format = json ? "json" : "table"
        let remoteCommand = "wp db query \"\(escapedSQL)\" --format=\(format) --path=\(appPath) --url=\(targetDomain)"
        
        let result = executeSSHCommand(
            host: serverConfig.host,
            user: serverConfig.user,
            serverId: serverId,
            command: remoteCommand
        )
        
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
    
    private func validateReadOnly(_ sql: String) throws {
        let forbidden = ["INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "TRUNCATE", "CREATE", "REPLACE", "GRANT", "REVOKE"]
        let upperSQL = sql.uppercased().trimmingCharacters(in: .whitespaces)
        
        for keyword in forbidden {
            if upperSQL.hasPrefix(keyword) {
                fputs("Error: Write operations not allowed via 'db query'.\n", stderr)
                fputs("Use 'homeboy wp <project> db query' for write operations.\n", stderr)
                throw ExitCode.failure
            }
        }
    }
}

// MARK: - Helpers

/// Resolves a potential blog nickname to a domain (WordPress multisite)
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

/// Validates WordPress project configuration and returns all needed values
func validateWordPressProject(projectId: String) throws -> (projectConfig: ProjectConfiguration, serverConfig: ServerConfig, wordpress: WordPressConfig, serverId: String) {
    guard let projectConfig = loadProjectConfig(id: projectId) else {
        fputs("Error: Project '\(projectId)' not found\n", stderr)
        throw ExitCode.failure
    }
    
    guard let serverId = projectConfig.serverId,
          let serverConfig = ConfigurationManager.readServer(id: serverId),
          !serverConfig.host.isEmpty,
          !serverConfig.user.isEmpty else {
        fputs("Error: Server not configured for project '\(projectId)'\n", stderr)
        throw ExitCode.failure
    }
    
    guard projectConfig.isWordPress,
          let wordpress = projectConfig.wordpress,
          wordpress.isConfigured else {
        fputs("Error: WordPress deployment not configured for project '\(projectId)'\n", stderr)
        throw ExitCode.failure
    }
    
    guard SSHService.ensureKeyFileExists(forServer: serverId) else {
        fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
        throw ExitCode.failure
    }
    
    return (projectConfig, serverConfig, wordpress, serverId)
}
