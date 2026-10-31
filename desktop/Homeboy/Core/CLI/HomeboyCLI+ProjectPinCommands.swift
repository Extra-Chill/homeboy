import Foundation

@MainActor
extension HomeboyCLI {
    func filePinAdd(projectId: String, path: String) async throws -> ProjectPinOutput {
        try await projectPin(
            HomeboyCommandRegistry.arguments("project pin add", appending: [projectId, path]),
            source: "File Pin Add"
        )
    }

    func filePinRemove(projectId: String, path: String) async throws -> ProjectPinOutput {
        try await projectPin(
            HomeboyCommandRegistry.arguments("project pin remove", appending: [projectId, path]),
            source: "File Pin Remove"
        )
    }

    func filePinUpdatePath(projectId: String, oldPath: String, newPath: String) async throws -> ProjectPinOutput {
        try await projectPin(
            HomeboyCommandRegistry.arguments("project pin rename", appending: [projectId, oldPath, newPath]),
            source: "File Pin Rename"
        )
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
