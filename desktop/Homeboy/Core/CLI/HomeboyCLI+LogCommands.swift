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

    func logPinAdd(projectId: String, path: String, tailLines: Int) async throws -> ProjectPinOutput {
        try await projectPin(
            ["project", "pin", "add", "--type", "log", "--tail", String(tailLines), projectId, path],
            source: "Log Pin Add"
        )
    }

    func logPinRemove(projectId: String, path: String) async throws -> ProjectPinOutput {
        try await projectPin(
            ["project", "pin", "remove", projectId, path, "--type", "log"],
            source: "Log Pin Remove"
        )
    }

    func logPinUpdateTail(projectId: String, path: String, tailLines: Int, previousTailLines: Int) async throws -> ProjectPinOutput {
        _ = try await logPinRemove(projectId: projectId, path: path)
        do {
            return try await logPinAdd(projectId: projectId, path: path, tailLines: tailLines)
        } catch {
            _ = try? await logPinAdd(projectId: projectId, path: path, tailLines: previousTailLines)
            throw error
        }
    }

    private func projectPin(_ args: [String], source: String) async throws -> ProjectPinOutput {
        let output: ProjectPinReportOutput = try await cli.executeCommand(
            args,
            dataType: ProjectPinReportOutput.self,
            source: source
        )
        guard let pin = output.pin else {
            throw CLIBridgeError.invalidResponse("\(source) response missing pin payload")
        }
        return pin
    }
}
