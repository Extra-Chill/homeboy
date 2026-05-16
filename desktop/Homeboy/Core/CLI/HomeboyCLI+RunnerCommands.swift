import Foundation

@MainActor
extension HomeboyCLI {
    func runnerList() async throws -> [HomeboyRunner] {
        let output: RunnerListOutput = try await cli.executeCommand(
            ["runner", "list"],
            dataType: RunnerListOutput.self,
            source: "Homeboy Lab",
            timeout: 30
        )

        let configured = output.entities
        if configured.contains(where: { $0.id == HomeboyRunner.localDefault.id }) {
            return configured
        }

        return [HomeboyRunner.localDefault] + configured
    }

    func runnerDoctor(id: String) async throws -> RunnerDoctorOutput {
        try await cli.executeCommand(
            ["runner", "doctor", id],
            dataType: RunnerDoctorOutput.self,
            source: "Homeboy Lab",
            timeout: 120
        )
    }

    func runnerConnect(id: String) async throws -> RunnerConnection {
        let output: RunnerConnectionOutput = try await cli.executeCommand(
            ["runner", "connect", id],
            dataType: RunnerConnectionOutput.self,
            source: "Homeboy Lab",
            timeout: 120
        )
        guard let connection = output.extra?.connection else {
            throw CLIBridgeError.invalidResponse("Runner connect response missing connection report")
        }
        return connection
    }

    func runnerStatus(id: String) async throws -> RunnerConnection {
        let output: RunnerConnectionOutput = try await cli.executeCommand(
            ["runner", "status", id],
            dataType: RunnerConnectionOutput.self,
            source: "Homeboy Lab",
            timeout: 30
        )
        guard let connection = output.extra?.connection else {
            throw CLIBridgeError.invalidResponse("Runner status response missing connection report")
        }
        return connection
    }

    func runnerDisconnect(id: String) async throws -> RunnerConnection {
        let output: RunnerConnectionOutput = try await cli.executeCommand(
            ["runner", "disconnect", id],
            dataType: RunnerConnectionOutput.self,
            source: "Homeboy Lab",
            timeout: 30
        )
        guard let connection = output.extra?.connection else {
            throw CLIBridgeError.invalidResponse("Runner disconnect response missing connection report")
        }
        return connection
    }

    func runsArtifactGet(runId: String, artifactId: String, outputPath: String) async throws {
        let response = try await cli.execute(
            ["runs", "artifact", "get", runId, artifactId, "--output", outputPath],
            timeout: 120
        )
        guard response.success else {
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }
    }
}
