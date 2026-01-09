import ArgumentParser
import Foundation

// MARK: - Project Command

struct Project: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "project",
        abstract: "Create and configure projects for deployment",
        discussion: """
            Create, update, and configure projects for deployment and remote operations.

            Common Operations:
              project create    Create a new project
              project set       Update project settings
              project show      Display project configuration
              project switch    Change active project

            Examples:
              homeboy project create "My Site" --type wordpress
              homeboy project set mysite --server production-1
              homeboy project show mysite --field domain

            See 'homeboy docs project' for full documentation.
            """,
        subcommands: [
            ProjectCreate.self,
            ProjectShow.self,
            ProjectSet.self,
            ProjectDelete.self,
            ProjectSwitch.self,
            ProjectDiscover.self,
            ProjectSubTarget.self,
            ProjectComponent.self
        ]
    )
}

// MARK: - Project Create

struct ProjectCreate: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "create",
        abstract: "Create a new project with specified type",
        discussion: """
            Creates a new project configuration file.

            Examples:
              homeboy project create "My Site" --type wordpress
              homeboy project create "API Server" --type nodejs --id api-prod

            After creation, configure server and paths with 'project set'.
            """
    )
    
    @Argument(help: "Project name")
    var name: String
    
    @Option(name: .long, help: "Project ID (default: auto-generated from name)")
    var id: String?
    
    @Option(name: .long, help: "Project type (e.g., wordpress, nodejs)")
    var type: String
    
    func run() throws {
        // Validate project type exists
        guard loadProjectTypeDefinition(id: type) != nil else {
            let available = getAvailableProjectTypeIds()
            fputs("Error: Unknown project type '\(type)'\n", stderr)
            if available.isEmpty {
                fputs("No project types available. Ensure Homeboy.app has been launched at least once.\n", stderr)
            } else {
                fputs("Available types: \(available.joined(separator: ", "))\n", stderr)
            }
            throw ExitCode.failure
        }
        
        let projectId = id ?? slugFromName(name)
        
        // Check if project already exists
        if loadProjectConfig(id: projectId) != nil {
            fputs("Error: Project '\(projectId)' already exists\n", stderr)
            throw ExitCode.failure
        }
        
        // Create new project
        let project = ProjectConfiguration.empty(id: projectId, name: name, projectType: type)
        try saveProjectConfig(project)
        
        // Output result
        let result: [String: Any] = [
            "success": true,
            "id": projectId,
            "name": name,
            "type": type
        ]
        print(formatJSON(result))
    }
}

// MARK: - Project Show

struct ProjectShow: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "show",
        abstract: "Display project configuration as JSON",
        discussion: """
            Displays project configuration. Use --field for specific values.

            Examples:
              homeboy project show mysite
              homeboy project show mysite --field domain
              homeboy project show mysite --field database.name
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Option(name: .long, help: "Specific field to show (supports dot notation, e.g., database.name)")
    var field: String?
    
    func run() throws {
        guard let project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        if let fieldPath = field {
            // Extract specific field using dot notation
            let value = extractField(from: project, path: fieldPath)
            print(value)
        } else {
            // Output full project JSON
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(project)
            print(String(data: data, encoding: .utf8) ?? "{}")
        }
    }
    
    private func extractField(from project: ProjectConfiguration, path: String) -> String {
        let components = path.split(separator: ".").map(String.init)
        
        // Handle top-level fields
        guard let first = components.first else { return "" }
        
        switch first {
        case "id": return project.id
        case "name": return project.name
        case "domain": return project.domain
        case "projectType": return project.projectType
        case "serverId": return project.serverId ?? ""
        case "basePath": return project.basePath ?? ""
        case "tablePrefix": return project.tablePrefix ?? ""
        case "database":
            if components.count == 1 {
                return encodeToJSON(project.database)
            }
            switch components[1] {
            case "name": return project.database.name
            case "user": return project.database.user
            case "host": return project.database.host
            case "port": return String(project.database.port)
            default: return ""
            }
        case "api":
            if components.count == 1 {
                return encodeToJSON(project.api)
            }
            switch components[1] {
            case "enabled": return String(project.api.enabled)
            case "baseURL": return project.api.baseURL
            default: return ""
            }
        case "localCLI", "localDev":  // localDev for backward compat
            if components.count == 1 {
                return encodeToJSON(project.localCLI)
            }
            switch components[1] {
            case "sitePath", "wpCliPath": return project.localCLI.sitePath  // wpCliPath for backward compat
            case "domain": return project.localCLI.domain
            case "cliPath": return project.localCLI.cliPath ?? ""
            default: return ""
            }
        case "subTargets":
            return encodeToJSON(project.subTargets)
        case "sharedTables":
            return encodeToJSON(project.sharedTables)
        case "components":
            return encodeToJSON(project.components)
        default:
            return ""
        }
    }
    
    private func encodeToJSON<T: Encodable>(_ value: T) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(value),
              let json = String(data: data, encoding: .utf8) else {
            return ""
        }
        return json
    }
}

// MARK: - Project Set

struct ProjectSet: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "set",
        abstract: "Update project domain, server, database, and path settings",
        discussion: """
            Modifies project settings. Supports dot notation for nested fields.

            Examples:
              homeboy project set mysite --domain example.com
              homeboy project set mysite --server production-1
              homeboy project set mysite --dbName wp_mysite --dbUser admin
              homeboy project set mysite --basePath /var/www/html

            Use 'project show <id>' to see current configuration.
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Option(name: .long, help: "Project domain")
    var domain: String?
    
    @Option(name: .long, help: "Server ID to link")
    var server: String?
    
    @Option(name: .long, help: "Remote base path")
    var basePath: String?
    
    @Option(name: .long, help: "Database table prefix")
    var tablePrefix: String?
    
    @Option(name: .long, help: "Project type")
    var type: String?
    
    @Option(name: .long, help: "Database name")
    var dbName: String?
    
    @Option(name: .long, help: "Database user")
    var dbUser: String?
    
    @Option(name: .long, help: "Database host")
    var dbHost: String?
    
    @Option(name: .long, help: "Database port")
    var dbPort: Int?
    
    @Option(name: .long, help: "Enable/disable API")
    var apiEnabled: Bool?
    
    @Option(name: .long, help: "API base URL")
    var apiUrl: String?
    
    @Option(name: .long, help: "Local WP-CLI path")
    var localWpCliPath: String?
    
    @Option(name: .long, help: "Local development domain")
    var localDomain: String?
    
    func run() throws {
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        var changes: [String] = []
        
        // Apply changes
        if let domain = domain {
            project.domain = domain
            changes.append("domain")
        }
        
        if let server = server {
            // Validate server exists
            guard ConfigurationManager.readServer(id: server) != nil else {
                fputs("Error: Server '\(server)' not found\n", stderr)
                throw ExitCode.failure
            }
            project.serverId = server
            changes.append("serverId")
        }
        
        if let basePath = basePath {
            project.basePath = basePath
            changes.append("basePath")
        }
        
        if let tablePrefix = tablePrefix {
            project.tablePrefix = tablePrefix
            changes.append("tablePrefix")
        }
        
        if let type = type {
            project.projectType = type
            changes.append("projectType")
        }
        
        if let dbName = dbName {
            project.database.name = dbName
            changes.append("database.name")
        }
        
        if let dbUser = dbUser {
            project.database.user = dbUser
            changes.append("database.user")
        }
        
        if let dbHost = dbHost {
            project.database.host = dbHost
            changes.append("database.host")
        }
        
        if let dbPort = dbPort {
            project.database.port = dbPort
            changes.append("database.port")
        }
        
        if let apiEnabled = apiEnabled {
            project.api.enabled = apiEnabled
            changes.append("api.enabled")
        }
        
        if let apiUrl = apiUrl {
            project.api.baseURL = apiUrl
            changes.append("api.baseURL")
        }
        
        if let localWpCliPath = localWpCliPath {
            project.localCLI.sitePath = localWpCliPath
            changes.append("localCLI.sitePath")
        }
        
        if let localDomain = localDomain {
            project.localCLI.domain = localDomain
            changes.append("localCLI.domain")
        }
        
        guard !changes.isEmpty else {
            fputs("Error: No changes specified\n", stderr)
            throw ExitCode.failure
        }
        
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "id": projectId,
            "updated": changes
        ]
        print(formatJSON(result))
    }
}

// MARK: - Project Delete

struct ProjectDelete: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "delete",
        abstract: "Permanently remove a project configuration",
        discussion: """
            Deletes a project configuration file. Requires --force flag.

            Example:
              homeboy project delete old-project --force

            Note: Cannot delete the active project. Use 'project switch' first.
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Flag(name: .long, help: "Confirm deletion")
    var force: Bool = false
    
    func run() throws {
        guard force else {
            fputs("Error: Use --force to confirm deletion\n", stderr)
            throw ExitCode.failure
        }
        
        guard loadProjectConfig(id: projectId) != nil else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Check if it's the active project
        let currentProject = ConfigurationManager.readCurrentProject()
        if currentProject.id == projectId {
            fputs("Error: Cannot delete active project. Switch to another project first.\n", stderr)
            throw ExitCode.failure
        }
        
        try deleteProjectConfig(id: projectId)
        
        let result: [String: Any] = [
            "success": true,
            "deleted": projectId
        ]
        print(formatJSON(result))
    }
}

// MARK: - Project Switch

struct ProjectSwitch: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "switch",
        abstract: "Set the active project for subsequent commands",
        discussion: """
            Sets the active project for commands that don't specify a project ID.

            Example:
              homeboy project switch client-site
              homeboy projects --current  # Verify active project
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    func run() throws {
        guard loadProjectConfig(id: projectId) != nil else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        try setActiveProject(id: projectId)
        
        let result: [String: Any] = [
            "success": true,
            "active": projectId
        ]
        print(formatJSON(result))
    }
}

// MARK: - Project Discover

struct ProjectDiscover: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "discover",
        abstract: "Auto-detect remote installation path via SSH",
        discussion: """
            Searches the remote server for installations and sets basePath.

            Examples:
              homeboy project discover mysite           # Interactive selection
              homeboy project discover mysite --list    # List without modifying
              homeboy project discover mysite --set /var/www/html

            Prerequisite: Server must be configured with SSH key.
            """
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Flag(name: .long, help: "List discovered installations without modifying config")
    var list: Bool = false
    
    @Option(name: .long, help: "Directly set basePath without discovery")
    var set: String?
    
    func run() throws {
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Direct set mode: skip discovery
        if let path = set {
            project.basePath = path
            try saveProjectConfig(project)
            let result: [String: Any] = [
                "success": true,
                "project": projectId,
                "basePath": path
            ]
            print(formatJSON(result))
            return
        }
        
        // Validate server is configured
        guard let serverId = project.serverId,
              let server = ConfigurationManager.readServer(id: serverId) else {
            fputs("Error: No server configured for project. Use 'homeboy project set \(projectId) --server <server-id>'\n", stderr)
            throw ExitCode.failure
        }
        
        // Load project type definition
        let typeDefinition = loadProjectTypeDefinition(id: project.projectType)
        guard let discovery = typeDefinition?.discovery else {
            fputs("Error: Discovery not supported for project type '\(project.projectType)'\n", stderr)
            throw ExitCode.failure
        }
        
        // Execute find command via SSH
        fputs("Searching for \(typeDefinition?.displayName ?? project.projectType) installations...\n", stderr)
        
        let findOutput = try runSSHCommandDirect(server: server, command: discovery.findCommand, ignoreExitCode: true)
        
        // Parse results and apply transform
        let foundPaths = findOutput
            .components(separatedBy: "\n")
            .map { $0.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .map { discovery.transformToBasePath($0) }
        
        // Deduplicate (multiple files might resolve to same basePath)
        let uniquePaths = Array(Set(foundPaths)).sorted()
        
        guard !uniquePaths.isEmpty else {
            fputs("No installations found\n", stderr)
            throw ExitCode.failure
        }
        
        // Get display names for each path
        var installations: [(path: String, name: String)] = []
        for path in uniquePaths {
            var displayName = path
            if let displayCmd = discovery.displayNameCommand {
                let cmd = displayCmd.replacingOccurrences(of: "{{basePath}}", with: path)
                if let nameOutput = try? runSSHCommandDirect(server: server, command: cmd),
                   !nameOutput.isEmpty {
                    displayName = nameOutput.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines)
                }
            }
            installations.append((path: path, name: displayName))
        }
        
        // List mode: just output and exit
        if list {
            let output = installations.map { ["path": $0.path, "name": $0.name] }
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            if let data = try? encoder.encode(output),
               let json = String(data: data, encoding: .utf8) {
                print(json)
            }
            return
        }
        
        // Interactive selection
        fputs("\nFound \(installations.count) installation(s):\n\n", stderr)
        for (index, install) in installations.enumerated() {
            let marker = install.name != install.path ? " (\(install.name))" : ""
            fputs("  [\(index + 1)] \(install.path)\(marker)\n", stderr)
        }
        fputs("\nEnter number to select (or 'q' to quit): ", stderr)
        
        guard let input = readLine()?.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines) else {
            fputs("Cancelled\n", stderr)
            throw ExitCode.failure
        }
        
        if input.lowercased() == "q" {
            fputs("Cancelled\n", stderr)
            throw ExitCode.failure
        }
        
        guard let selection = Int(input), selection >= 1, selection <= installations.count else {
            fputs("Invalid selection\n", stderr)
            throw ExitCode.failure
        }
        
        let selectedPath = installations[selection - 1].path
        project.basePath = selectedPath
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "basePath": selectedPath
        ]
        print(formatJSON(result))
    }
}

/// Run SSH command synchronously and return stdout (CLI-safe, no dispatch queues)
/// Set ignoreExitCode to true for commands like find that may return non-zero but still produce valid output
private func runSSHCommandDirect(server: ServerConfig, command: String, ignoreExitCode: Bool = false) throws -> String {
    // Ensure SSH key exists for this server
    guard SSHService.ensureKeyFileExists(forServer: server.id) else {
        throw NSError(domain: "ProjectDiscover", code: 1, userInfo: [NSLocalizedDescriptionKey: "SSH key not found for server"])
    }
    
    let keyPath = SSHService.keyPath(forServer: server.id)
    
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
    process.arguments = [
        "-i", keyPath,
        "-o", "StrictHostKeyChecking=no",
        "-o", "BatchMode=yes",
        "\(server.user)@\(server.host)",
        command
    ]
    
    let outputPipe = Pipe()
    let errorPipe = Pipe()
    process.standardOutput = outputPipe
    process.standardError = errorPipe
    
    try process.run()
    process.waitUntilExit()
    
    let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
    let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
    let output = String(data: outputData, encoding: .utf8) ?? ""
    let errorOutput = String(data: errorData, encoding: .utf8) ?? ""
    
    // For discovery commands, we may get non-zero exit codes (e.g., find with permission denied)
    // but still have valid output
    if process.terminationStatus != 0 && !ignoreExitCode {
        throw NSError(domain: "ProjectDiscover", code: Int(process.terminationStatus), 
                      userInfo: [NSLocalizedDescriptionKey: errorOutput.isEmpty ? output : errorOutput])
    }
    
    return output
}

// MARK: - Project SubTarget

struct ProjectSubTarget: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "subtarget",
        abstract: "Configure multisite blogs or environment targets",
        discussion: """
            Manage subtargets for WordPress multisite or multi-environment setups.

            Examples:
              homeboy project subtarget add mysite shop --name "Shop" --domain shop.example.com
              homeboy project subtarget list mysite
              homeboy project subtarget set mysite shop --isDefault
            """,
        subcommands: [
            SubTargetAdd.self,
            SubTargetRemove.self,
            SubTargetList.self,
            SubTargetSet.self
        ]
    )
}

struct SubTargetAdd: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "add",
        abstract: "Add a subtarget to a project"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(help: "Subtarget ID (slug)")
    var subTargetId: String
    
    @Option(name: .long, help: "Display name")
    var name: String
    
    @Option(name: .long, help: "Domain")
    var domain: String
    
    @Option(name: .long, help: "Numeric ID (e.g., WordPress blog_id)")
    var number: Int?
    
    @Flag(name: .long, help: "Set as default subtarget")
    var isDefault: Bool = false
    
    func run() throws {
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        // Check if subtarget already exists
        if project.subTargets.contains(where: { $0.id == subTargetId }) {
            fputs("Error: Subtarget '\(subTargetId)' already exists\n", stderr)
            throw ExitCode.failure
        }
        
        // If setting as default, clear other defaults
        if isDefault {
            for i in project.subTargets.indices {
                project.subTargets[i].isDefault = false
            }
        }
        
        let subTarget = SubTarget(
            id: subTargetId,
            name: name,
            domain: domain,
            number: number,
            isDefault: isDefault || project.subTargets.isEmpty  // First subtarget is default
        )
        
        project.subTargets.append(subTarget)
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "subtarget": subTargetId
        ]
        print(formatJSON(result))
    }
}

struct SubTargetRemove: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "remove",
        abstract: "Remove a subtarget from a project"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(help: "Subtarget ID")
    var subTargetId: String
    
    @Flag(name: .long, help: "Confirm deletion")
    var force: Bool = false
    
    func run() throws {
        guard force else {
            fputs("Error: Use --force to confirm removal\n", stderr)
            throw ExitCode.failure
        }
        
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        guard let index = project.subTargets.firstIndex(where: { $0.id == subTargetId }) else {
            fputs("Error: Subtarget '\(subTargetId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        let wasDefault = project.subTargets[index].isDefault
        project.subTargets.remove(at: index)
        
        // If removed subtarget was default, make first one default
        if wasDefault && !project.subTargets.isEmpty {
            project.subTargets[0].isDefault = true
        }
        
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "removed": subTargetId
        ]
        print(formatJSON(result))
    }
}

struct SubTargetList: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "list",
        abstract: "List project subtargets"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    func run() throws {
        guard let project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(project.subTargets)
        print(String(data: data, encoding: .utf8) ?? "[]")
    }
}

struct SubTargetSet: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "set",
        abstract: "Update subtarget fields"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(help: "Subtarget ID")
    var subTargetId: String
    
    @Option(name: .long, help: "Display name")
    var name: String?
    
    @Option(name: .long, help: "Domain")
    var domain: String?
    
    @Option(name: .long, help: "Numeric ID")
    var number: Int?
    
    @Flag(name: .long, help: "Set as default subtarget")
    var isDefault: Bool = false
    
    func run() throws {
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        guard let index = project.subTargets.firstIndex(where: { $0.id == subTargetId }) else {
            fputs("Error: Subtarget '\(subTargetId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        var changes: [String] = []
        
        if let name = name {
            project.subTargets[index].name = name
            changes.append("name")
        }
        
        if let domain = domain {
            project.subTargets[index].domain = domain
            changes.append("domain")
        }
        
        if let number = number {
            project.subTargets[index].number = number
            changes.append("number")
        }
        
        if isDefault {
            // Clear other defaults
            for i in project.subTargets.indices {
                project.subTargets[i].isDefault = false
            }
            project.subTargets[index].isDefault = true
            changes.append("isDefault")
        }
        
        guard !changes.isEmpty else {
            fputs("Error: No changes specified\n", stderr)
            throw ExitCode.failure
        }
        
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "subtarget": subTargetId,
            "updated": changes
        ]
        print(formatJSON(result))
    }
}

// MARK: - Project Component

struct ProjectComponent: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "component",
        abstract: "Configure deployable plugins, themes, or packages",
        discussion: """
            Manage deployment components (plugins, themes, packages).

            Examples:
              homeboy project component add mysite "My Plugin" --localPath ~/plugins/my-plugin --remotePath plugins/my-plugin --buildArtifact dist/my-plugin.zip
              homeboy project component list mysite
              homeboy project component remove mysite my-plugin --force
            """,
        subcommands: [
            ComponentAdd.self,
            ComponentRemove.self,
            ComponentList.self
        ]
    )
}

struct ComponentAdd: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "add",
        abstract: "Add a component to a project"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(help: "Component name")
    var name: String
    
    @Option(name: .long, help: "Local path to component source")
    var localPath: String
    
    @Option(name: .long, help: "Remote path (relative to basePath)")
    var remotePath: String
    
    @Option(name: .long, help: "Build artifact path (relative to localPath)")
    var buildArtifact: String
    
    @Option(name: .long, help: "Version file (relative to localPath)")
    var versionFile: String?
    
    @Option(name: .long, help: "Version regex pattern")
    var versionPattern: String?
    
    func run() throws {
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        let id = slugFromName(name)
        
        // Check if component already exists
        if project.components.contains(where: { $0.id == id }) {
            fputs("Error: Component '\(id)' already exists\n", stderr)
            throw ExitCode.failure
        }
        
        let component = ComponentConfig(
            id: id,
            name: name,
            localPath: localPath,
            remotePath: remotePath,
            buildArtifact: buildArtifact,
            versionFile: versionFile,
            versionPattern: versionPattern
        )
        
        project.components.append(component)
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "component": id
        ]
        print(formatJSON(result))
    }
}

struct ComponentRemove: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "remove",
        abstract: "Remove a component from a project"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    @Argument(help: "Component ID")
    var componentId: String
    
    @Flag(name: .long, help: "Confirm deletion")
    var force: Bool = false
    
    func run() throws {
        guard force else {
            fputs("Error: Use --force to confirm removal\n", stderr)
            throw ExitCode.failure
        }
        
        guard var project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        guard let index = project.components.firstIndex(where: { $0.id == componentId }) else {
            fputs("Error: Component '\(componentId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        project.components.remove(at: index)
        try saveProjectConfig(project)
        
        let result: [String: Any] = [
            "success": true,
            "project": projectId,
            "removed": componentId
        ]
        print(formatJSON(result))
    }
}

struct ComponentList: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "list",
        abstract: "List project components"
    )
    
    @Argument(help: "Project ID")
    var projectId: String
    
    func run() throws {
        guard let project = loadProjectConfig(id: projectId) else {
            fputs("Error: Project '\(projectId)' not found\n", stderr)
            throw ExitCode.failure
        }
        
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(project.components)
        print(String(data: data, encoding: .utf8) ?? "[]")
    }
}

// MARK: - Helper Functions

/// Generate a slug from a name
private func slugFromName(_ name: String) -> String {
    name.lowercased()
        .trimmingCharacters(in: .whitespaces)
        .replacingOccurrences(of: " ", with: "-")
        .replacingOccurrences(of: "[^a-z0-9-]", with: "", options: .regularExpression)
        .replacingOccurrences(of: "--+", with: "-", options: .regularExpression)
        .trimmingCharacters(in: CharacterSet(charactersIn: "-"))
}

/// Save project configuration to disk
private func saveProjectConfig(_ project: ProjectConfiguration) throws {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let projectPath = appSupport.appendingPathComponent("Homeboy/projects/\(project.id).json")
    
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let data = try encoder.encode(project)
    try data.write(to: projectPath)
}

/// Delete project configuration from disk
private func deleteProjectConfig(id: String) throws {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let projectPath = appSupport.appendingPathComponent("Homeboy/projects/\(id).json")
    try fileManager.removeItem(at: projectPath)
}

/// Set active project in app config
private func setActiveProject(id: String) throws {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let configPath = appSupport.appendingPathComponent("Homeboy/config.json")
    
    var config = AppConfiguration()
    if let data = try? Data(contentsOf: configPath),
       let existing = try? JSONDecoder().decode(AppConfiguration.self, from: data) {
        config = existing
    }
    
    config.activeProjectId = id
    
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let data = try encoder.encode(config)
    try data.write(to: configPath)
}

/// Format dictionary as JSON string
private func formatJSON(_ dict: [String: Any]) -> String {
    guard let data = try? JSONSerialization.data(withJSONObject: dict, options: [.prettyPrinted, .sortedKeys]),
          let json = String(data: data, encoding: .utf8) else {
        return "{}"
    }
    return json
}

/// Get available project type IDs from Application Support
private func getAvailableProjectTypeIds() -> [String] {
    let fileManager = FileManager.default
    let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    let typesDir = appSupport.appendingPathComponent("Homeboy/project-types")
    
    guard let files = try? fileManager.contentsOfDirectory(at: typesDir, includingPropertiesForKeys: nil) else {
        return []
    }
    
    return files
        .filter { $0.pathExtension == "json" }
        .map { $0.deletingPathExtension().lastPathComponent }
        .sorted()
}
