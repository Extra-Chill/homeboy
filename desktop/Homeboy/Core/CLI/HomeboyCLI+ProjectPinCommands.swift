import Foundation

@MainActor
extension HomeboyCLI {
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

    func filePinAdd(projectId: String, path: String) async throws -> ProjectPinOutput {
        try await projectPin(
            ["project", "pin", "add", projectId, path, "--type", "file"],
            source: "File Pin Add"
        )
    }

    func filePinRemove(projectId: String, path: String) async throws -> ProjectPinOutput {
        try await projectPin(
            ["project", "pin", "remove", projectId, path, "--type", "file"],
            source: "File Pin Remove"
        )
    }

    func filePinUpdatePath(projectId: String, oldPath: String, newPath: String) async throws -> ProjectPinOutput {
        _ = try await filePinRemove(projectId: projectId, path: oldPath)
        do {
            return try await filePinAdd(projectId: projectId, path: newPath)
        } catch {
            _ = try? await filePinAdd(projectId: projectId, path: oldPath)
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
