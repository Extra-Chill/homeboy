import Foundation

@MainActor
extension HomeboyCLI {
    func logsList(projectId: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "list", projectId],
            dataType: LogsOutput.self,
            source: "Logs List"
        )
    }

    func logsShow(projectId: String, path: String, lines: Int) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "show", projectId, path, "-n", String(lines)],
            dataType: LogsOutput.self,
            source: "Logs Show"
        )
    }

    func logsClear(projectId: String, path: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "clear", projectId, path],
            dataType: LogsOutput.self,
            source: "Logs Clear"
        )
    }

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
            args.append(contentsOf: ["-n", String(lines)])
        }
        if let context {
            args.append(contentsOf: ["-C", String(context)])
        }
        return try await cli.executeCommand(args, dataType: LogsOutput.self, source: "Logs Search", timeout: 60)
    }

}
