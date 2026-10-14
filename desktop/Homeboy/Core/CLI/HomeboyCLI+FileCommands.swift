import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - File Commands

    func fileList(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("file list", appending: [projectId, path]),
            dataType: FileOutput.self,
            source: "File List"
        )
    }

    func fileRead(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("file read", appending: [projectId, path]),
            dataType: FileOutput.self,
            source: "File Read",
            timeout: 60
        )
    }

    func fileWrite(projectId: String, path: String, content: String) async throws -> FileOutput {
        let response = try await cli.executeWithStdin(HomeboyCommandRegistry.arguments("file write", appending: [projectId, path]), stdin: content, timeout: 60)
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
        var args = HomeboyCommandRegistry.arguments("file delete", appending: [projectId, path])
        if recursive {
            args.append("--recursive")
        }
        return try await cli.executeCommand(args, dataType: FileOutput.self, source: "File Delete")
    }

    func fileRename(projectId: String, oldPath: String, newPath: String) async throws -> FileOutput {
        try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("file rename", appending: [projectId, oldPath, newPath]),
            dataType: FileOutput.self,
            source: "File Rename"
        )
    }

    func sshCommand(projectId: String, command: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("ssh", appending: [projectId, command]), timeout: 30)
    }

    func serverKeyGenerate(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("server key generate", appending: [serverId]), timeout: 60)
    }

    func serverKeyShow(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("server key show", appending: [serverId]), timeout: 30)
    }

    func serverKeyUnset(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("server key unset", appending: [serverId]), timeout: 30)
    }

    func serverKeyImport(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("server key import", appending: [serverId, privateKeyPath]), timeout: 60)
    }

    func serverKeyUse(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(HomeboyCommandRegistry.arguments("server key use", appending: [serverId, privateKeyPath]), timeout: 30)
    }

    // MARK: - File Search

    func fileFind(
        projectId: String,
        path: String,
        namePattern: String? = nil,
        fileType: String? = nil,
        maxDepth: Int? = nil
    ) async throws -> FileFindOutput {
        var args = HomeboyCommandRegistry.arguments("file find", appending: [projectId, path])
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
        var args = HomeboyCommandRegistry.arguments("file grep", appending: [projectId, path, pattern])
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
