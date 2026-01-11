import Foundation

// MARK: - CLI Output Models

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

    private init() {}

    func fileList(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeJSON(["file", "list", projectId, path, "--json"], as: FileOutput.self)
    }

    func fileRead(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeJSON(["file", "read", projectId, path, "--json"], as: FileOutput.self)
    }

    func fileWrite(projectId: String, path: String, content: String) async throws -> FileOutput {
        let response = try await cli.executeWithStdin(["file", "write", projectId, path, "--json"], stdin: content, timeout: 60)
        return try response.decode(FileOutput.self)
    }

    func fileDelete(projectId: String, path: String, recursive: Bool) async throws -> FileOutput {
        var args = ["file", "delete", projectId, path, "--json"]
        if recursive {
            args.append("--recursive")
        }
        return try await cli.executeJSON(args, as: FileOutput.self)
    }

    func fileRename(projectId: String, oldPath: String, newPath: String) async throws -> FileOutput {
        try await cli.executeJSON(["file", "rename", projectId, oldPath, newPath, "--json"], as: FileOutput.self)
    }

    func logsList(projectId: String) async throws -> LogsOutput {
        try await cli.executeJSON(["logs", "list", projectId, "--json"], as: LogsOutput.self)
    }

    func logsShow(projectId: String, path: String, lines: Int?) async throws -> LogsOutput {
        var args = ["logs", "show", projectId, path, "--json"]
        if let lines {
            args.append(contentsOf: ["-n", "\(lines)"])
        }
        return try await cli.executeJSON(args, as: LogsOutput.self)
    }

    func logsClear(projectId: String, path: String) async throws -> LogsOutput {
        try await cli.executeJSON(["logs", "clear", projectId, path, "--json"], as: LogsOutput.self)
    }

    func sshCommand(projectId: String, command: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["ssh", projectId, command], timeout: 30)
    }

    func dbTables(projectId: String) async throws -> DbOutput {
        try await cli.executeJSON(["db", "tables", projectId, "--json"], as: DbOutput.self)
    }

    func dbDescribe(projectId: String, table: String?) async throws -> DbOutput {
        var args = ["db", "describe", projectId]
        if let table {
            args.append(table)
        }
        args.append("--json")
        return try await cli.executeJSON(args, as: DbOutput.self)
    }

    func dbQuery(projectId: String, sql: String) async throws -> DbOutput {
        try await cli.executeJSON(["db", "query", projectId, sql, "--json"], as: DbOutput.self)
    }

    func dbDeleteRow(projectId: String, table: String, rowId: String) async throws -> DbOutput {
        try await cli.executeJSON(["db", "delete-row", projectId, table, rowId, "--confirm", "--json"], as: DbOutput.self)
    }

    func dbDropTable(projectId: String, table: String) async throws -> DbOutput {
        try await cli.executeJSON(["db", "drop-table", projectId, table, "--confirm", "--json"], as: DbOutput.self)
    }
}
