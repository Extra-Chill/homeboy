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
}
