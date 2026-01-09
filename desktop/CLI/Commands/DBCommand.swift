import ArgumentParser
import Foundation

struct DB: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "db",
        abstract: "Query production databases (read-only)",
        discussion: """
            Query and inspect production databases via WP-CLI over SSH.

            Subcommands:
              tables     List database tables
              describe   Show table structure
              query      Execute SQL query (SELECT only)

            Examples:
              homeboy db extrachill tables
              homeboy db extrachill describe wp_posts
              homeboy db extrachill query "SELECT * FROM wp_users LIMIT 10"

            Note: Write operations are blocked. Use 'homeboy wp db query' for writes.

            See 'homeboy docs db' for full documentation.
            """,
        subcommands: [DBTables.self, DBDescribe.self, DBQuery.self]
    )
}

// MARK: - DBTables

struct DBTables: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "tables",
        abstract: "List all tables in the project database",
        discussion: """
            Lists all tables in the project's database.

            Examples:
              homeboy db extrachill tables
              homeboy db extrachill tables --json
              homeboy db extrachill shop tables  # Multisite subtarget
            """
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional subtarget ID")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        let context = try DatabaseCLI.buildContext(
            projectId: projectId,
            args: args,
            json: json
        )
        
        let command = TemplateRenderer.render(
            context.cliConfig.tablesCommand,
            variables: context.variables
        )
        
        try DatabaseCLI.execute(command: command, context: context)
    }
}

// MARK: - DBDescribe

struct DBDescribe: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "describe",
        abstract: "Display column definitions for a table",
        discussion: """
            Displays column definitions for a database table.

            Examples:
              homeboy db extrachill describe wp_posts
              homeboy db extrachill describe wp_options --json
            """
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by table name")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        var context = try DatabaseCLI.buildContext(
            projectId: projectId,
            args: args,
            json: json
        )
        
        // Extract table name from remaining args
        guard let tableName = context.remainingArgs.first else {
            fputs("Error: Table name required\n", stderr)
            fputs("Usage: homeboy db <project> describe <table>\n", stderr)
            throw ExitCode.failure
        }
        
        // Add table to variables
        context.variables[TemplateRenderer.table] = tableName
        
        let command = TemplateRenderer.render(
            context.cliConfig.describeCommand,
            variables: context.variables
        )
        
        try DatabaseCLI.execute(command: command, context: context)
    }
}

// MARK: - DBQuery

struct DBQuery: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "query",
        abstract: "Run SELECT queries against production database",
        discussion: """
            Runs a SELECT query against the production database.

            Examples:
              homeboy db extrachill query "SELECT * FROM wp_users LIMIT 10"
              homeboy db extrachill query "SELECT post_status, COUNT(*) FROM wp_posts GROUP BY post_status"
              homeboy db extrachill shop query "SELECT * FROM wp_2_posts LIMIT 5"

            Blocked: INSERT, UPDATE, DELETE, DROP, ALTER, TRUNCATE, CREATE, REPLACE
            For write operations, use 'homeboy wp <project> db query'.
            """
    )
    
    @Argument(help: "Project ID (e.g., extrachill)")
    var projectId: String
    
    @Argument(parsing: .captureForPassthrough, help: "Optional blog nickname followed by SQL query")
    var args: [String] = []
    
    @Flag(name: .long, help: "Output as JSON")
    var json: Bool = false
    
    func run() throws {
        var context = try DatabaseCLI.buildContext(
            projectId: projectId,
            args: args,
            json: json
        )
        
        // Join remaining args as the SQL query
        let sql = context.remainingArgs.joined(separator: " ")
        
        guard !sql.isEmpty else {
            fputs("Error: SQL query required\n", stderr)
            fputs("Usage: homeboy db <project> query \"SELECT * FROM table_name\"\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate read-only query
        try validateReadOnly(sql)
        
        // Escape the SQL for shell and add to variables
        let escapedSQL = sql.replacingOccurrences(of: "\"", with: "\\\"")
        context.variables[TemplateRenderer.query] = escapedSQL
        
        let command = TemplateRenderer.render(
            context.cliConfig.queryCommand,
            variables: context.variables
        )
        
        try DatabaseCLI.execute(command: command, context: context)
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

// MARK: - DatabaseCLI Helper

/// Shared logic for database CLI commands
enum DatabaseCLI {
    
    /// Context containing all information needed to execute a database command
    struct Context {
        let project: ProjectConfiguration
        let server: ServerConfig
        let serverId: String
        let cliConfig: DatabaseCLIConfig
        var variables: [String: String]
        var remainingArgs: [String]
    }
    
    /// Builds the execution context for a database command
    static func buildContext(
        projectId: String,
        args: [String],
        json: Bool
    ) throws -> Context {
        // Load project
        guard let project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate server configuration
        guard let serverId = project.serverId,
              let server = ConfigurationManager.readServer(id: serverId),
              !server.host.isEmpty,
              !server.user.isEmpty else {
            fputs("Error: Server not configured for project '\(projectId)'. Configure in Homeboy.app Settings.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate SSH key exists
        guard SSHService.ensureKeyFileExists(forServer: serverId) else {
            fputs("Error: SSH key not found for server. Configure SSH in Homeboy.app first.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate database CLI is configured for this project type
        guard let cliConfig = project.typeDefinition.database?.cli else {
            fputs("Error: Project type '\(project.typeDefinition.displayName)' does not support database CLI commands.\n", stderr)
            fputs("Add a 'database.cli' block to the project type definition.\n", stderr)
            throw ExitCode.failure
        }
        
        // Validate database credentials for non-WordPress projects
        if !project.isWordPress {
            try validateDatabaseCredentials(project)
        }
        
        // Build template variables
        var variables = buildVariables(project: project, json: json)
        
        // Handle subtarget resolution (e.g., WordPress multisite blogs, environments)
        var remainingArgs = args
        if project.hasSubTargets {
            let (targetDomain, wasSubTarget) = resolveSubTargetDomain(
                project: project,
                potentialSubTarget: args.first ?? ""
            )
            variables[TemplateRenderer.targetDomain] = targetDomain
            
            if wasSubTarget {
                remainingArgs.removeFirst()
            }
        }
        
        return Context(
            project: project,
            server: server,
            serverId: serverId,
            cliConfig: cliConfig,
            variables: variables,
            remainingArgs: remainingArgs
        )
    }
    
    /// Validates that database credentials are configured (for non-WordPress projects)
    private static func validateDatabaseCredentials(_ project: ProjectConfiguration) throws {
        guard !project.database.name.isEmpty else {
            fputs("Error: Database name not configured. Set it in Homeboy.app Settings → Database.\n", stderr)
            throw ExitCode.failure
        }
        
        guard !project.database.user.isEmpty else {
            fputs("Error: Database user not configured. Set it in Homeboy.app Settings → Database.\n", stderr)
            throw ExitCode.failure
        }
        
        guard KeychainService.hasLiveMySQLCredentials() else {
            fputs("Error: Database password not configured. Set it in Homeboy.app Settings → Database.\n", stderr)
            throw ExitCode.failure
        }
    }
    
    /// Builds template variables for database CLI commands
    private static func buildVariables(project: ProjectConfiguration, json: Bool) -> [String: String] {
        var vars: [String: String] = [:]
        
        // Standard variables
        vars[TemplateRenderer.projectId] = project.id
        vars[TemplateRenderer.domain] = project.domain
        vars[TemplateRenderer.targetDomain] = project.domain
        vars[TemplateRenderer.basePath] = project.basePath ?? ""
        
        // Format
        vars[TemplateRenderer.format] = json ? "json" : "table"
        
        // Database credentials (for non-WordPress direct MySQL access)
        vars[TemplateRenderer.dbName] = project.database.name
        vars[TemplateRenderer.dbHost] = project.database.host.isEmpty ? "localhost" : project.database.host
        vars[TemplateRenderer.dbUser] = project.database.user
        
        // Password from Keychain (shell-escaped for safety)
        if let password = KeychainService.getLiveMySQLCredentials().password {
            vars[TemplateRenderer.dbPassword] = TemplateRenderer.shellEscape(password)
        } else {
            vars[TemplateRenderer.dbPassword] = ""
        }
        
        return vars
    }
    
    /// Executes the rendered command via SSH
    static func execute(command: String, context: Context) throws {
        let result = executeSSHCommand(
            host: context.server.host,
            user: context.server.user,
            serverId: context.serverId,
            command: command
        )
        
        print(result.output, terminator: "")
        
        if !result.success {
            throw ExitCode.failure
        }
    }
}

// MARK: - SubTarget Helper

/// Resolves a potential subtarget ID to a domain (e.g., WordPress multisite blog, environment)
private func resolveSubTargetDomain(
    project: ProjectConfiguration,
    potentialSubTarget: String
) -> (domain: String, wasSubTarget: Bool) {
    guard project.hasSubTargets else {
        return (project.domain, false)
    }
    
    // Case-insensitive match against subtarget IDs or names
    if let subTarget = project.subTargets.first(where: {
        $0.id.lowercased() == potentialSubTarget.lowercased() ||
        $0.name.lowercased() == potentialSubTarget.lowercased()
    }) {
        return (subTarget.domain, true)
    }
    
    return (project.domain, false)
}
