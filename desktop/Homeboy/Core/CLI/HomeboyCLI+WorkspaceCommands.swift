import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Workspace Status Command

    /// Run `homeboy status --full` to get full context including extensions and components
    /// This is the primary way the desktop app discovers the workspace state
    func initWorkspace() async throws -> InitOutput {
        let args = ["status", "--full"]
        return try await cli.executeCommand(args, dataType: InitOutput.self, source: "Status", timeout: 30)
    }

    // MARK: - Git Commands

    /// Read-only wrapper for `homeboy git status`.
    func gitStatus(componentId: String, path: String? = nil) async throws -> GitStatusOutput {
        var args = ["git", "status", componentId]
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
            ["project", "list"],
            dataType: ProjectListOutput.self,
            source: "Project List"
        )
        return output.projects ?? []
    }

    func projectShow(id: String) async throws -> ProjectShowOutput {
        let output: ProjectShowOutput = try await cli.executeCommand(
            ["project", "show", id],
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
        var args = ["project", "create", name, domain]
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
            ["project", "set", id, "--json", json],
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
            ["project", "delete", id],
            dataType: ProjectMutationOutput.self,
            source: "Project Delete"
        )
    }

    // MARK: - API/Auth Commands

    func authStatus(projectId: String) async throws -> HomeboyAuthOutput {
        try await cli.executeCommand(
            ["auth", "status", "--project", projectId],
            dataType: HomeboyAuthOutput.self,
            source: "API Auth Status"
        )
    }

    func authLogin(projectId: String, identifier: String, password: String) async throws -> HomeboyAuthOutput {
        try await cli.executeCommand(
            ["auth", "login", "--project", projectId, "--identifier", identifier, "--password", password],
            dataType: HomeboyAuthOutput.self,
            source: "API Auth Login",
            timeout: 60
        )
    }

    func authLogout(projectId: String) async throws -> HomeboyAuthOutput {
        try await cli.executeCommand(
            ["auth", "logout", "--project", projectId],
            dataType: HomeboyAuthOutput.self,
            source: "API Auth Logout"
        )
    }

    func apiGet(projectId: String, endpoint: String) async throws -> HomeboyAPIGetOutput {
        try await cli.executeCommand(
            ["api", projectId, "get", endpoint],
            dataType: HomeboyAPIGetOutput.self,
            source: "API GET",
            timeout: 60
        )
    }

    // MARK: - Server Commands

    func serverList() async throws -> [ServerListItemCLI] {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "list"],
            dataType: ServerOutput.self,
            source: "Server List"
        )
        return output.servers ?? []
    }

    func serverShow(id: String) async throws -> ServerRecordCLI {
        let output: ServerOutput = try await cli.executeCommand(
            ["server", "show", id],
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
            ["server", "create", name, host, user, "--port", String(port)],
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
            ["server", "set", id, "--json", json],
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
            ["server", "delete", id],
            dataType: ServerOutput.self,
            source: "Server Delete"
        )
    }

    // MARK: - Fleet Commands

    /// List all fleets
    func fleetList() async throws -> [Fleet] {
        let output: FleetListOutput = try await cli.executeCommand(
            ["fleet", "list"],
            dataType: FleetListOutput.self,
            source: "Fleet List"
        )
        return output.fleets ?? []
    }

    /// Show fleet details
    func fleetShow(id: String) async throws -> Fleet {
        let output: FleetOutput = try await cli.executeCommand(
            ["fleet", "show", id],
            dataType: FleetOutput.self,
            source: "Fleet Show"
        )
        guard let fleet = output.fleet else {
            throw CLIBridgeError.invalidResponse("Fleet not found: \(id)")
        }
        return fleet
    }

    /// Create a new fleet
    func fleetCreate(id: String, description: String? = nil, projectIds: [String] = []) async throws -> Fleet {
        var args = ["fleet", "create", id]
        if let desc = description {
            args += ["--description", desc]
        }
        if !projectIds.isEmpty {
            args += ["--projects", projectIds.joined(separator: ",")]
        }
        let output: FleetOutput = try await cli.executeCommand(
            args,
            dataType: FleetOutput.self,
            source: "Fleet Create"
        )
        guard let fleet = output.fleet else {
            throw CLIBridgeError.invalidResponse("Failed to create fleet")
        }
        return fleet
    }

    /// Delete a fleet
    func fleetDelete(id: String) async throws {
        _ = try await cli.executeCommand(
            ["fleet", "delete", id],
            dataType: FleetOutput.self,
            source: "Fleet Delete"
        )
    }

    /// Add project to fleet
    func fleetAddProject(fleetId: String, projectId: String) async throws -> Fleet {
        let output: FleetOutput = try await cli.executeCommand(
            ["fleet", "add", fleetId, projectId],
            dataType: FleetOutput.self,
            source: "Fleet Add Project"
        )
        guard let fleet = output.fleet else {
            throw CLIBridgeError.invalidResponse("Failed to add project to fleet")
        }
        return fleet
    }

    /// Remove project from fleet
    func fleetRemoveProject(fleetId: String, projectId: String) async throws -> Fleet {
        let output: FleetOutput = try await cli.executeCommand(
            ["fleet", "remove", fleetId, projectId],
            dataType: FleetOutput.self,
            source: "Fleet Remove Project"
        )
        guard let fleet = output.fleet else {
            throw CLIBridgeError.invalidResponse("Failed to remove project from fleet")
        }
        return fleet
    }

    /// Get projects in fleet
    func fleetProjects(fleetId: String) async throws -> [ProjectListItem] {
        let output: FleetProjectsOutput = try await cli.executeCommand(
            ["fleet", "projects", fleetId],
            dataType: FleetProjectsOutput.self,
            source: "Fleet Projects"
        )
        return output.projects ?? []
    }

    /// Get component usage across fleet
    func fleetComponents(fleetId: String) async throws -> [String: [String]] {
        let output: FleetComponentsOutput = try await cli.executeCommand(
            ["fleet", "components", fleetId],
            dataType: FleetComponentsOutput.self,
            source: "Fleet Components"
        )
        return output.components ?? [:]
    }

    /// Check fleet status (versions and health)
    func fleetStatus(fleetId: String) async throws -> FleetStatusOutput {
        try await cli.executeCommand(
            ["fleet", "status", fleetId],
            dataType: FleetStatusOutput.self,
            source: "Fleet Status",
            timeout: 60
        )
    }

    /// Check component drift across fleet
    func fleetCheck(fleetId: String) async throws -> FleetCheckOutput {
        try await cli.executeCommand(
            ["fleet", "check", fleetId],
            dataType: FleetCheckOutput.self,
            source: "Fleet Check",
            timeout: 60
        )
    }

    /// Execute command across all projects in fleet
    func fleetExec(fleetId: String, command: String) async throws -> FleetExecOutput {
        try await cli.executeCommand(
            ["fleet", "exec", fleetId, "--", command],
            dataType: FleetExecOutput.self,
            source: "Fleet Exec",
            timeout: 120
        )
    }

// MARK: - Component Commands

    func componentList() async throws -> [ComponentListItemCLI] {
        let output: ComponentOutput = try await cli.executeCommand(
            ["component", "list"],
            dataType: ComponentOutput.self,
            source: "Component List"
        )
        return output.components ?? []
    }

    func componentShow(id: String) async throws -> ComponentRecordCLI {
        let output: ComponentOutput = try await cli.executeCommand(
            ["component", "show", id],
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
        var args = [
            "component", "create",
            "--local-path", localPath,
            "--remote-path", remotePath,
        ]
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
            ["component", "set", id, "--json", json],
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
            ["component", "delete", id],
            dataType: ComponentOutput.self,
            source: "Component Delete"
        )
    }

}
