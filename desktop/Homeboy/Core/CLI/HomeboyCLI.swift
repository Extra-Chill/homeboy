import Foundation

// Note: `CLIBridge` is defined in `Homeboy/Core/CLI/CLIBridge.swift`.


// MARK: - Project CLI Output Models

/// Output from `homeboy project list`
struct ProjectListOutput: Decodable {
    let command: String
    let projects: [ProjectListItem]?
}

/// Summary item from project list
struct ProjectListItem: Decodable, Identifiable {
    let id: String
    let name: String
    let domain: String
    let modules: [String]
}

/// Output from `homeboy project show <id>`
struct ProjectShowOutput: Decodable {
    let command: String
    let project: ProjectRecord?
    let projectId: String?
}

/// Full project record with id and config
struct ProjectRecord: Decodable {
    let id: String
    let config: ProjectConfig
}

/// Project configuration matching CLI's Project struct
struct ProjectConfig: Decodable {
    let name: String
    let domain: String
    let modules: [String]
    let scopedModules: [String: ScopedModuleConfig]?
    let serverId: String?
    let basePath: String?
    let tablePrefix: String?
    let remoteFiles: RemoteFileConfigCLI
    let remoteLogs: RemoteLogConfigCLI
    let database: DatabaseConfigCLI
    let tools: ToolsConfigCLI
    let api: ApiConfigCLI
    let changelogNextSectionLabel: String?
    let changelogNextSectionAliases: [String]?
    let subTargets: [SubTargetCLI]
    let sharedTables: [String]
    let componentIds: [String]
}

struct ScopedModuleConfig: Decodable {
    let settings: [String: AnyCodableValue]?
}

struct RemoteFileConfigCLI: Decodable {
    let pinnedFiles: [PinnedRemoteFileCLI]
}

struct PinnedRemoteFileCLI: Decodable {
    let id: String
    let path: String
    let label: String?
}

struct RemoteLogConfigCLI: Decodable {
    let pinnedLogs: [PinnedRemoteLogCLI]
}

struct PinnedRemoteLogCLI: Decodable {
    let id: String
    let path: String
    let label: String?
    let tailLines: Int
}

struct DatabaseConfigCLI: Decodable {
    let host: String
    let port: Int
    let name: String
    let user: String
    let useSshTunnel: Bool
}

struct ToolsConfigCLI: Decodable {
    let bandcampScraper: BandcampScraperConfig?
    let newsletter: NewsletterToolConfig?

    struct BandcampScraperConfig: Decodable {
        let defaultTag: String?
    }

    struct NewsletterToolConfig: Decodable {
        let sendyListId: String?
    }
}

struct ApiConfigCLI: Decodable {
    let baseUrl: String
    let enabled: Bool
}

struct SubTargetCLI: Decodable, Identifiable {
    let name: String
    let domain: String
    let number: Int
    let isDefault: Bool

    var id: String { String(number) }
}

// MARK: - File CLI Output Models

struct FileListEntry: Decodable {
    let name: String
    let path: String
    let isDirectory: Bool
    let size: Int64?
    let permissions: String?
}

struct FileOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String?
    let oldPath: String?
    let newPath: String?
    let recursive: Bool?
    let entries: [FileListEntry]?
    let content: String?
    let bytesWritten: Int?
    let exitCode: Int32?
    let success: Bool?
}

struct LogsListEntry: Decodable {
    let path: String
    let label: String?
    let tailLines: Int?
}

struct LogsTail: Decodable {
    let path: String
    let lines: Int?
    let content: String
}

struct LogsOutput: Decodable {
    let command: String
    let projectId: String
    let entries: [LogsListEntry]?
    let log: LogsTail?
    let clearedPath: String?
    let searchResult: LogSearchResult?
}

// MARK: - File Search Types

struct FileFindOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String
    let pattern: String?
    let matches: [String]
    let matchCount: Int
}

struct FileGrepOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String
    let pattern: String
    let matches: [FileGrepMatch]
    let matchCount: Int
}

struct FileGrepMatch: Decodable {
    let file: String
    let line: Int
    let content: String
}

// MARK: - Logs Search Types

struct LogSearchResult: Decodable {
    let path: String
    let pattern: String
    let matches: [LogSearchMatch]
    let matchCount: Int
}

struct LogSearchMatch: Decodable {
    let lineNumber: Int
    let content: String
}

struct DbTunnelInfo: Decodable {
    let localPort: Int
    let remoteHost: String
    let remotePort: Int
    let database: String
    let user: String
}

struct DbOutput: Decodable {
    let command: String
    let projectId: String
    let exitCode: Int32?
    let success: Bool?
    let stdout: String?
    let stderr: String?

    let tables: [String]?
    let table: String?
    let sql: String?
    let tunnel: DbTunnelInfo?
}

// MARK: - Server CLI Output Models

struct ServerOutput: Decodable {
    let command: String
    let serverId: String?
    let server: ServerRecord?
    let servers: [ServerListItem]?
    let updated: [String]?
    let deleted: [String]?
}

struct ServerListItem: Decodable, Identifiable {
    let id: String
    let name: String
    let host: String
    let port: Int
    let user: String
    let identityFile: String?
}

struct ServerRecord: Decodable {
    let id: String
    let name: String
    let host: String
    let port: Int
    let user: String
    let identityFile: String?
}

// MARK: - Component CLI Output Models

struct ComponentOutput: Decodable {
    let command: String
    let componentId: String?
    let component: ComponentRecord?
    let components: [ComponentListItem]?
    let updated: [String]?
    let deleted: [String]?
}

struct ComponentListItem: Decodable, Identifiable {
    let id: String
    let name: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
}

struct ComponentRecord: Decodable {
    let id: String
    let name: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
    let versionTargets: [VersionTarget]?

    struct VersionTarget: Decodable {
        let file: String
        let pattern: String?
    }
}

// MARK: - Project Mutation Output Models

struct ProjectMutationOutput: Decodable {
    let command: String
    let projectId: String?
    let project: ProjectRecord?
    let updated: [String]?
    let deleted: [String]?
}

@MainActor
final class HomeboyCLI {
    static let shared = HomeboyCLI()

    private let cli = CLIBridge.shared

    var isInstalled: Bool {
        cli.isInstalled
    }

    private init() {}

    // MARK: - Project Commands

    func projectList() async throws -> [ProjectListItem] {
        let output: ProjectListOutput = try await cli.executeCommand(
            ["project", "list"],
            dataType: ProjectListOutput.self,
            source: "Project List"
        )
        return output.projects ?? []
    }

    func projectShow(id: String) async throws -> ProjectRecord {
        let output: ProjectShowOutput = try await cli.executeCommand(
            ["project", "show", id],
            dataType: ProjectShowOutput.self,
            source: "Project Show"
        )
        guard let project = output.project else {
            throw CLIBridgeError.invalidResponse("Project not found: \(id)")
        }
        return project
    }

    func projectCreate(
        name: String,
        domain: String,
        serverId: String? = nil,
        basePath: String? = nil,
        tablePrefix: String? = nil
    ) async throws -> ProjectRecord {
        var args = ["project", "create", name, domain]
        if let serverId {
            args.append(contentsOf: ["--server-id", serverId])
        }
        if let basePath {
            args.append(contentsOf: ["--base-path", basePath])
        }
        if let tablePrefix {
            args.append(contentsOf: ["--table-prefix", tablePrefix])
        }
        let output: ProjectMutationOutput = try await cli.executeCommand(
            args,
            dataType: ProjectMutationOutput.self,
            source: "Project Create"
        )
        guard let project = output.project else {
            throw CLIBridgeError.invalidResponse("Project creation failed")
        }
        return project
    }

    func projectSet(id: String, json: String) async throws -> ProjectRecord {
        let output: ProjectMutationOutput = try await cli.executeCommand(
            ["project", "set", id, "--json", json],
            dataType: ProjectMutationOutput.self,
            source: "Project Set"
        )
        guard let project = output.project else {
            throw CLIBridgeError.invalidResponse("Project update failed")
        }
        return project
    }

    func projectDelete(id: String) async throws {
        let _: ProjectMutationOutput = try await cli.executeCommand(
            ["project", "delete", id],
            dataType: ProjectMutationOutput.self,
            source: "Project Delete"
        )
    }

    // MARK: - Server Commands

    func serverList() async throws -> [ServerListItem] {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "list"],
            dataType: ServerOutput.self,
            source: "Server List"
        )
        return output.servers ?? []
    }

    func serverShow(id: String) async throws -> ServerRecord {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "show", id],
            dataType: ServerOutput.self,
            source: "Server Show"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server not found: \(id)")
        }
        return server
    }

    func serverCreate(
        name: String,
        host: String,
        user: String,
        port: Int = 22
    ) async throws -> ServerRecord {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "create", name, host, user, "--port", String(port)],
            dataType: ServerOutput.self,
            source: "Server Create"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server creation failed")
        }
        return server
    }

    func serverSet(id: String, json: String) async throws -> ServerRecord {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "set", id, "--json", json],
            dataType: ServerOutput.self,
            source: "Server Set"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server update failed")
        }
        return server
    }

    func serverDelete(id: String) async throws {
        let _: ServerOutput = try await cli.executeCommand(
            ["server", "delete", id],
            dataType: ServerOutput.self,
            source: "Server Delete"
        )
    }

    // MARK: - Component Commands

    func componentList() async throws -> [ComponentListItem] {
        let output: ComponentOutput = try await cli.executeCommand(
            ["component", "list"],
            dataType: ComponentOutput.self,
            source: "Component List"
        )
        return output.components ?? []
    }

    func componentShow(id: String) async throws -> ComponentRecord {
        let output: ComponentOutput = try await cli.executeCommand(
            ["component", "show", id],
            dataType: ComponentOutput.self,
            source: "Component Show"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component not found: \(id)")
        }
        return component
    }

    func componentCreate(
        name: String,
        localPath: String,
        remotePath: String,
        buildArtifact: String? = nil
    ) async throws -> ComponentRecord {
        var args = ["component", "create", name, localPath, remotePath]
        if let buildArtifact {
            args.append(contentsOf: ["--build-artifact", buildArtifact])
        }
        let output: ComponentOutput = try await cli.executeCommand(
            args,
            dataType: ComponentOutput.self,
            source: "Component Create"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component creation failed")
        }
        return component
    }

    func componentSet(id: String, json: String) async throws -> ComponentRecord {
        let output: ComponentOutput = try await cli.executeCommand(
            ["component", "set", id, "--json", json],
            dataType: ComponentOutput.self,
            source: "Component Set"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component update failed")
        }
        return component
    }

    func componentDelete(id: String) async throws {
        let _: ComponentOutput = try await cli.executeCommand(
            ["component", "delete", id],
            dataType: ComponentOutput.self,
            source: "Component Delete"
        )
    }

    // MARK: - File Commands

    func fileList(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "list", projectId, path],
            dataType: FileOutput.self,
            source: "File List"
        )
    }

    func fileRead(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "read", projectId, path],
            dataType: FileOutput.self,
            source: "File Read",
            timeout: 60
        )
    }

    func fileWrite(projectId: String, path: String, content: String) async throws -> FileOutput {
        let response = try await cli.executeWithStdin(["file", "write", projectId, path], stdin: content, timeout: 60)
        let result = try response.decodeResponse(FileOutput.self)
        guard result.success else {
            if let errorDetail = result.error {
                throw CLIBridgeError.cliError(errorDetail.toCLIError(source: "File Write"))
            }
            throw CLIBridgeError.executionFailed(exitCode: 1, message: "Unknown error")
        }
        guard let data = result.data else {
            throw CLIBridgeError.invalidResponse("Success response missing data")
        }
        return data
    }

    func fileDelete(projectId: String, path: String, recursive: Bool) async throws -> FileOutput {
        var args = ["file", "delete", projectId, path]
        if recursive {
            args.append("--recursive")
        }
        return try await cli.executeCommand(args, dataType: FileOutput.self, source: "File Delete")
    }

    func fileRename(projectId: String, oldPath: String, newPath: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "rename", projectId, oldPath, newPath],
            dataType: FileOutput.self,
            source: "File Rename"
        )
    }

    func logsList(projectId: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "list", projectId],
            dataType: LogsOutput.self,
            source: "Logs List"
        )
    }

    func logsShow(projectId: String, path: String, lines: Int?) async throws -> LogsOutput {
        var args = ["logs", "show", projectId, path]
        if let lines {
            args.append(contentsOf: ["-n", "\(lines)"])
        }
        return try await cli.executeCommand(args, dataType: LogsOutput.self, source: "Logs Show")
    }

    func logsClear(projectId: String, path: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "clear", projectId, path],
            dataType: LogsOutput.self,
            source: "Logs Clear"
        )
    }

    func sshCommand(projectId: String, command: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["ssh", projectId, command], timeout: 30)
    }

    func serverKeyGenerate(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "generate", serverId], timeout: 60)
    }

    func serverKeyShow(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "show", serverId], timeout: 30)
    }

    func serverKeyUnset(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "unset", serverId], timeout: 30)
    }

    func serverKeyImport(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "import", serverId, privateKeyPath], timeout: 60)
    }

    func serverKeyUse(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "use", serverId, privateKeyPath], timeout: 30)
    }

    func dbTables(projectId: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "tables", projectId],
            dataType: DbOutput.self,
            source: "Database Tables"
        )
    }

    func dbDescribe(projectId: String, table: String?) async throws -> DbOutput {
        var args = ["db", "describe", projectId]
        if let table {
            args.append(table)
        }
        return try await cli.executeCommand(args, dataType: DbOutput.self, source: "Database Describe")
    }

    func dbQuery(projectId: String, sql: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "query", projectId, sql],
            dataType: DbOutput.self,
            source: "Database Query",
            timeout: 60
        )
    }

    func dbDeleteRow(projectId: String, table: String, rowId: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "delete-row", projectId, table, rowId, "--confirm"],
            dataType: DbOutput.self,
            source: "Database Delete Row"
        )
    }

    func dbDropTable(projectId: String, table: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "drop-table", projectId, table, "--confirm"],
            dataType: DbOutput.self,
            source: "Database Drop Table"
        )
    }

    func dbSearch(
        projectId: String,
        table: String,
        column: String,
        pattern: String,
        exact: Bool = false,
        limit: Int? = nil,
        subtarget: String? = nil
    ) async throws -> DbOutput {
        var args = ["db", "search", projectId, table, "--column", column, "--pattern", pattern]
        if exact {
            args.append("--exact")
        }
        if let limit {
            args.append(contentsOf: ["--limit", "\(limit)"])
        }
        if let subtarget {
            args.append(contentsOf: ["--subtarget", subtarget])
        }
        return try await cli.executeCommand(args, dataType: DbOutput.self, source: "Database Search", timeout: 60)
    }

    // MARK: - File Search

    func fileFind(
        projectId: String,
        path: String,
        namePattern: String? = nil,
        fileType: String? = nil,
        maxDepth: Int? = nil
    ) async throws -> FileFindOutput {
        var args = ["file", "find", projectId, path]
        if let name = namePattern {
            args.append(contentsOf: ["--name", name])
        }
        if let type = fileType {
            args.append(contentsOf: ["--type", type])
        }
        if let depth = maxDepth {
            args.append(contentsOf: ["--max-depth", "\(depth)"])
        }
        return try await cli.executeCommand(args, dataType: FileFindOutput.self, source: "File Find", timeout: 60)
    }

    func fileGrep(
        projectId: String,
        path: String,
        pattern: String,
        nameFilter: String? = nil,
        maxDepth: Int? = nil,
        caseInsensitive: Bool = false
    ) async throws -> FileGrepOutput {
        var args = ["file", "grep", projectId, path, pattern]
        if let name = nameFilter {
            args.append(contentsOf: ["--name", name])
        }
        if let depth = maxDepth {
            args.append(contentsOf: ["--max-depth", "\(depth)"])
        }
        if caseInsensitive {
            args.append("-i")
        }
        return try await cli.executeCommand(args, dataType: FileGrepOutput.self, source: "File Grep", timeout: 60)
    }

    // MARK: - Logs Search

    func logsSearch(
        projectId: String,
        path: String,
        pattern: String,
        caseInsensitive: Bool = false,
        lines: Int? = nil,
        context: Int? = nil
    ) async throws -> LogsOutput {
        var args = ["logs", "search", projectId, path, pattern]
        if caseInsensitive {
            args.append("-i")
        }
        if let lines {
            args.append(contentsOf: ["-n", "\(lines)"])
        }
        if let context {
            args.append(contentsOf: ["-C", "\(context)"])
        }
        return try await cli.executeCommand(args, dataType: LogsOutput.self, source: "Logs Search", timeout: 60)
    }
}
