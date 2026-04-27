import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Rig Commands

    func rigList() async throws -> [RigListItem] {
        let output: RigListOutput = try await cli.executeCommand(
            ["rig", "list"],
            dataType: RigListOutput.self,
            source: "Rig List"
        )
        return output.rigs ?? []
    }

    func rigShow(id: String) async throws -> RigSpec {
        let output: RigShowOutput = try await cli.executeCommand(
            ["rig", "show", id],
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
            ["rig", "status", id],
            dataType: RigStatusOutput.self,
            source: "Rig Status",
            timeout: 60
        )
    }

    func rigCheck(id: String) async throws -> RigCommandResult<RigCheckOutput> {
        try await runRigCommand(["rig", "check", id], dataType: RigCheckOutput.self, source: "Rig Check", timeout: 180)
    }

    func rigUp(id: String) async throws -> RigCommandResult<RigLifecycleOutput> {
        try await runRigCommand(["rig", "up", id], dataType: RigLifecycleOutput.self, source: "Rig Up", timeout: 900)
    }

    func rigDown(id: String) async throws -> RigCommandResult<RigLifecycleOutput> {
        try await runRigCommand(["rig", "down", id], dataType: RigLifecycleOutput.self, source: "Rig Down", timeout: 300)
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
        var args = ["bench", "list", componentId]
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: BenchCommandOutput.self, source: "Bench List", timeout: 60)
    }

    func benchRun(componentId: String, path: String? = nil, iterations: Int = 10) async throws -> BenchCommandOutput {
        var args = ["bench", componentId, "--iterations", String(iterations)]
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: BenchCommandOutput.self, source: "Bench", timeout: 300)
    }

    // MARK: - Release / Build Planning Commands

    /// Inspect changes since the release baseline for a component.
    func changes(componentId: String) async throws -> ChangesOutput {
        try await cli.executeCommand(
            ["changes", componentId],
            dataType: ChangesOutput.self,
            source: "Release Changes",
            timeout: 60
        )
    }

    /// Show the current version for a component. Path is optional and only used for local override workflows.
    func versionShow(componentId: String, path: String? = nil) async throws -> VersionShowOutput {
        var args = ["version", "show", componentId]
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: VersionShowOutput.self, source: "Version Show", timeout: 30)
    }

    /// Execute a component build through the CLI. This is intentionally separate from release execution.
    func build(componentId: String, path: String? = nil) async throws -> BuildOutput {
        var args = ["build", componentId]
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: BuildOutput.self, source: "Build", timeout: 300)
    }

    /// Preview a release plan without mutating the repository.
    func releaseDryRun(componentId: String, path: String? = nil) async throws -> ReleaseOutput {
        var args = ["release", componentId, "--dry-run"]
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        return try await cli.executeCommand(args, dataType: ReleaseOutput.self, source: "Release Dry Run", timeout: 120)
    }

    // MARK: - Undo Command

    /// Undo the last write operation
    func undo(snapshotId: String? = nil) async throws -> UndoOutput {
        var args = ["undo"]
        if let id = snapshotId {
            args.append(contentsOf: ["--id", id])
        }
        return try await cli.executeCommand(args, dataType: UndoOutput.self, source: "Undo", timeout: 30)
    }

    /// List available undo snapshots
    func undoList() async throws -> [UndoSnapshot] {
        let output: UndoListOutput = try await cli.executeCommand(
            ["undo", "list"],
            dataType: UndoListOutput.self,
            source: "Undo List",
            timeout: 10
        )
        return output.snapshots ?? []
    }

}
