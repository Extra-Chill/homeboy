import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Workspace Status Command

    /// Run `homeboy status --full` to get full context including extensions and components
    /// This is the primary way the desktop app discovers the workspace state
    func initWorkspace() async throws -> InitOutput {
        let args = HomeboyCommandRegistry.arguments("status", appending: ["--full"])
        return try await cli.executeCommand(args, dataType: InitOutput.self, source: "Status", timeout: 30)
    }

    // MARK: - Git Commands

    /// Read-only wrapper for `homeboy git status`.
    func gitStatus(componentId: String, path: String? = nil) async throws -> GitStatusOutput {
        var args = HomeboyCommandRegistry.arguments("git status", appending: [componentId])
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }

        return try await cli.executeCommand(
            args,
            dataType: GitStatusOutput.self,
            source: "Git Status",
            timeout: 30
        )
    }

    // MARK: - Config Gap Commands

    /// Fix a config gap by executing the command provided by CLI
    /// The command is parsed from gap.command (e.g., "homeboy component set foo --extension nodejs")
    func fixConfigGap(_ gap: ConfigGapDetail) async throws -> ComponentOutput {
        // Parse the command from gap.command
        // Format: "homeboy component set <id> --<field> <value>" or similar
        let commandParts = gap.command.split(separator: " ").map(String.init)

        // Remove "homeboy" prefix if present
        let args = commandParts.first == "homeboy" ? Array(commandParts.dropFirst()) : commandParts

        return try await cli.executeCommand(
            args,
            dataType: ComponentOutput.self,
            source: "Fix Config Gap",
            timeout: 30
        )
    }

    /// Run init to refresh and return current config gaps
    func refreshConfigGaps() async throws -> [ConfigGapDetail] {
        let initOutput = try await initWorkspace()
        return initOutput.status.gapDetails ?? []
    }

// MARK: - Project Commands

    func projectList() async throws -> [ProjectListItem] {
        let output: ProjectListOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("project list"),
            dataType: ProjectListOutput.self,
            source: "Project List"
        )
        return output.projects ?? []
    }

    func projectShow(id: String) async throws -> ProjectShowOutput {
        let output: ProjectShowOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("project show", appending: [id]),
            dataType: ProjectShowOutput.self,
            source: "Project Show"
        )
        guard output.project != nil else {
            throw CLIBridgeError.invalidResponse("Project not found: \(id)")
        }
        return output
    }

    func projectCreate(
        name: String,
        domain: String,
        serverId: String? = nil,
        basePath: String? = nil,
        tablePrefix: String? = nil
    ) async throws -> ProjectMutationOutput {
        var args = HomeboyCommandRegistry.arguments("project create", appending: [name, domain])
        if let serverId {
            args.append(contentsOf: ["--server-id", serverId])
        }
        if let basePath {
            args.append(contentsOf: ["--base-path", basePath])
        }
        if let tablePrefix {
            args.append(contentsOf: ["--table-prefix", tablePrefix])
        }
        let output: ProjectMutationOutput = try await cli.executeCommand(
            args,
            dataType: ProjectMutationOutput.self,
            source: "Project Create"
        )
        guard output.project != nil else {
            throw CLIBridgeError.invalidResponse("Project creation failed")
        }
        return output
    }

    func projectSet(id: String, json: String) async throws -> ProjectMutationOutput {
        let output: ProjectMutationOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("project set", appending: [id, "--json", json]),
            dataType: ProjectMutationOutput.self,
            source: "Project Set"
        )
        guard output.project != nil else {
            throw CLIBridgeError.invalidResponse("Project update failed")
        }
        return output
    }

    func projectDelete(id: String) async throws {
        let _: ProjectMutationOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("project delete", appending: [id]),
            dataType: ProjectMutationOutput.self,
            source: "Project Delete"
        )
    }

    // MARK: - Server Commands

    func serverList() async throws -> [ServerListItemCLI] {
        let output: ServerOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("server list"),
            dataType: ServerOutput.self,
            source: "Server List"
        )
        return output.servers ?? []
    }

    func serverShow(id: String) async throws -> ServerRecordCLI {
        let output: ServerOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("server show", appending: [id]),
            dataType: ServerOutput.self,
            source: "Server Show"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server not found: \(id)")
        }
        return server
    }

    func serverCreate(
        name: String,
        host: String,
        user: String,
        port: Int = 22
    ) async throws -> ServerRecordCLI {
        let output: ServerOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("server create", appending: [name, host, user, "--port", String(port)]),
            dataType: ServerOutput.self,
            source: "Server Create"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server creation failed")
        }
        return server
    }

    func serverSet(id: String, json: String) async throws -> ServerRecordCLI {
        let output: ServerOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("server set", appending: [id, "--json", json]),
            dataType: ServerOutput.self,
            source: "Server Set"
        )
        guard let server = output.server else {
            throw CLIBridgeError.invalidResponse("Server update failed")
        }
        return server
    }

    func serverDelete(id: String) async throws {
        let _: ServerOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("server delete", appending: [id]),
            dataType: ServerOutput.self,
            source: "Server Delete"
        )
    }

// MARK: - Component Commands

    func componentList() async throws -> [ComponentListItemCLI] {
        let output: ComponentOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("component list"),
            dataType: ComponentOutput.self,
            source: "Component List"
        )
        return output.components ?? []
    }

    func componentShow(id: String) async throws -> ComponentRecordCLI {
        let output: ComponentOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("component show", appending: [id]),
            dataType: ComponentOutput.self,
            source: "Component Show"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component not found: \(id)")
        }
        return component
    }

    func componentCreate(
        name: String,
        localPath: String,
        remotePath: String,
        buildArtifact: String? = nil
    ) async throws -> ComponentRecordCLI {
        var args = HomeboyCommandRegistry.arguments(
            "component create",
            appending: ["--local-path", localPath, "--remote-path", remotePath]
        )
        if let buildArtifact {
            args.append(contentsOf: ["--build-artifact", buildArtifact])
        }
        let output: ComponentOutput = try await cli.executeCommand(
            args,
            dataType: ComponentOutput.self,
            source: "Component Create"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component creation failed")
        }
        return component
    }

    func componentSet(id: String, json: String) async throws -> ComponentRecordCLI {
        let output: ComponentOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("component set", appending: [id, "--json", json]),
            dataType: ComponentOutput.self,
            source: "Component Set"
        )
        guard let component = output.component else {
            throw CLIBridgeError.invalidResponse("Component update failed")
        }
        return component
    }

    func componentDelete(id: String) async throws {
        let _: ComponentOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("component delete", appending: [id]),
            dataType: ComponentOutput.self,
            source: "Component Delete"
        )
    }

}
