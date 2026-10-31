import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Rig Commands

    func rigList() async throws -> [RigListItem] {
        let output: RigListOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("rig list"),
            dataType: RigListOutput.self,
            source: "Rig List"
        )
        return output.rigs ?? []
    }

    func rigShow(id: String) async throws -> RigSpec {
        let output: RigShowOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("rig show", appending: [id]),
            dataType: RigShowOutput.self,
            source: "Rig Show"
        )
        guard let rig = output.rig else {
            throw CLIBridgeError.invalidResponse("Rig not found: \(id)")
        }
        return rig
    }

    func rigStatus(id: String) async throws -> RigStatusOutput {
        try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("rig status", appending: [id]),
            dataType: RigStatusOutput.self,
            source: "Rig Status",
            timeout: 60
        )
    }

    func rigCheck(id: String) async throws -> RigCommandResult<RigCheckOutput> {
        try await runRigCommand(HomeboyCommandRegistry.arguments("rig check", appending: [id]), dataType: RigCheckOutput.self, source: "Rig Check", timeout: 180)
    }

    func rigUp(id: String) async throws -> RigCommandResult<RigLifecycleOutput> {
        try await runRigCommand(HomeboyCommandRegistry.arguments("rig up", appending: [id]), dataType: RigLifecycleOutput.self, source: "Rig Up", timeout: 900)
    }

    func rigDown(id: String) async throws -> RigCommandResult<RigLifecycleOutput> {
        try await runRigCommand(HomeboyCommandRegistry.arguments("rig down", appending: [id]), dataType: RigLifecycleOutput.self, source: "Rig Down", timeout: 300)
    }

    private func runRigCommand<T: Decodable>(
        _ args: [String],
        dataType: T.Type,
        source: String,
        timeout: TimeInterval
    ) async throws -> RigCommandResult<T> {
        let outputURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("homeboy-rig-\(UUID().uuidString).json")
        var commandArgs = args
        if commandArgs.first == "rig" {
            commandArgs.insert(contentsOf: ["--output", outputURL.path], at: 1)
        }

        let response = try await cli.execute(commandArgs, timeout: timeout)
        let structuredOutput = (try? String(contentsOf: outputURL, encoding: .utf8)).flatMap { value in
            value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : value
        } ?? response.output
        try? FileManager.default.removeItem(at: outputURL)

        guard let data = structuredOutput.data(using: .utf8) else {
            throw CLIBridgeError.invalidResponse("\(source) output is not valid UTF-8")
        }

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let result = try decoder.decode(CLIBridgeResult<T>.self, from: data)

        if let data = result.data {
            return RigCommandResult(
                success: result.success,
                data: data,
                rawOutput: response.output.isEmpty ? structuredOutput : response.output,
                errorOutput: response.errorOutput,
                exitCode: response.exitCode
            )
        }

        if let errorDetail = result.error {
            throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
        }

        throw CLIBridgeError.invalidResponse("\(source) response missing data")
    }

    // MARK: - Bench Commands

    func benchList(componentId: String, path: String? = nil) async throws -> BenchCommandOutput {
        var args = HomeboyCommandRegistry.arguments("bench list", appending: [componentId])
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: BenchCommandOutput.self, source: "Bench List", timeout: 60)
    }

    func benchRun(componentId: String, path: String? = nil, iterations: Int = 10) async throws -> BenchCommandOutput {
        var args = HomeboyCommandRegistry.arguments("bench", appending: [componentId, "--iterations", String(iterations)])
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: BenchCommandOutput.self, source: "Bench", timeout: 300)
    }

    // MARK: - Release / Build Planning Commands

    func releaseChanges(componentId: String) async throws -> ChangesOutput {
        try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("release changes", appending: [componentId]),
            dataType: ChangesOutput.self,
            source: "Release Changes",
            timeout: 60
        )
    }

    func releaseVersionShow(componentId: String, path: String? = nil) async throws -> VersionShowOutput {
        var args = HomeboyCommandRegistry.arguments("release version show", appending: [componentId])
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: VersionShowOutput.self, source: "Release Version Show", timeout: 30)
    }

    func reviewBuild(componentId: String, path: String? = nil) async throws {
        var args = HomeboyCommandRegistry.arguments("review build", appending: [componentId])
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        let response = try await cli.execute(args, timeout: 300)
        guard response.success else {
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }
    }

    func releaseDryRun(componentId: String, path: String? = nil) async throws -> ReleaseOutput {
        var args = HomeboyCommandRegistry.arguments("release", appending: [componentId, "--dry-run"])
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: ReleaseOutput.self, source: "Release Dry Run", timeout: 120)
    }
}
