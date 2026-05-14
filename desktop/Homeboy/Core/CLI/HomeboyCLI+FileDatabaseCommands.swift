import Foundation

@MainActor
extension HomeboyCLI {
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
        return try await cli.executeCommand(args, dataType: DbOutput.self, source: "Database Describe", timeout: 60)
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
            args.append(contentsOf: ["--limit", String(limit)])
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
            args.append(contentsOf: ["--file-type", type])
        }
        if let depth = maxDepth {
            args.append(contentsOf: ["--max-depth", String(depth)])
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
            args.append(contentsOf: ["--max-depth", String(depth)])
        }
        if caseInsensitive {
            args.append("-i")
        }
        return try await cli.executeCommand(args, dataType: FileGrepOutput.self, source: "File Grep", timeout: 60)
    }

}
