import Foundation

// Note: `CLIBridge` is defined in `Homeboy/Core/CLI/CLIBridge.swift`.


// MARK: - Project CLI Output Models

/// Output from `homeboy project list`
struct ProjectListOutput: Decodable {
    let command: String
    let projects: [ProjectListItem]?
}

/// Summary item from project list (matches CLI output)
struct ProjectListItem: Decodable, Identifiable {
    let id: String
    let domain: String?
}

/// Output from `homeboy project show <id>`
struct ProjectShowOutput: Decodable {
    let command: String
    let project: ProjectConfigCLI?
    let projectId: String?
}

/// Project configuration matching CLI's Project struct (no wrapper)
struct ProjectConfigCLI: Decodable {
    let domain: String?
    let serverId: String?
    let basePath: String?
    let tablePrefix: String?
    let changelogNextSectionLabel: String?
    let changelogNextSectionAliases: [String]?
    let componentIds: [String]
    let components: [ProjectComponentAttachmentCLI]
    let componentOverrides: [String: ProjectComponentOverrides]
    let services: [String]
    let remoteFiles: RemoteFileConfigCLI
    let remoteLogs: RemoteLogConfigCLI
    let database: DatabaseConfigCLI
    let tools: ToolsConfigCLI
    let api: ApiConfigCLI
    let subTargets: [SubTargetCLI]
    let sharedTables: [String]

    private enum CodingKeys: String, CodingKey {
        case domain, serverId, basePath, tablePrefix
        case changelogNextSectionLabel, changelogNextSectionAliases
        case componentIds, components, componentOverrides, services
        case remoteFiles, remoteLogs, database, tools, api, subTargets, sharedTables
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)

        domain = try container.decodeIfPresent(String.self, forKey: .domain)
        serverId = try container.decodeIfPresent(String.self, forKey: .serverId)
        basePath = try container.decodeIfPresent(String.self, forKey: .basePath)
        tablePrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix)
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
        components = try container.decodeIfPresent([ProjectComponentAttachmentCLI].self, forKey: .components) ?? []
        componentIds = try container.decodeIfPresent([String].self, forKey: .componentIds)
            ?? components.map { $0.id }
        componentOverrides = try container.decodeIfPresent([String: ProjectComponentOverrides].self, forKey: .componentOverrides) ?? [:]
        services = try container.decodeIfPresent([String].self, forKey: .services) ?? []
        remoteFiles = try container.decodeIfPresent(RemoteFileConfigCLI.self, forKey: .remoteFiles) ?? RemoteFileConfigCLI()
        remoteLogs = try container.decodeIfPresent(RemoteLogConfigCLI.self, forKey: .remoteLogs) ?? RemoteLogConfigCLI()
        database = try container.decodeIfPresent(DatabaseConfigCLI.self, forKey: .database) ?? DatabaseConfigCLI()
        tools = try container.decodeIfPresent(ToolsConfigCLI.self, forKey: .tools) ?? ToolsConfigCLI()
        api = try container.decodeIfPresent(ApiConfigCLI.self, forKey: .api) ?? ApiConfigCLI()
        subTargets = try container.decodeIfPresent([SubTargetCLI].self, forKey: .subTargets) ?? []
        sharedTables = try container.decodeIfPresent([String].self, forKey: .sharedTables) ?? []
    }
}

struct ProjectComponentAttachmentCLI: Decodable {
    let id: String
    let localPath: String

    private enum CodingKeys: String, CodingKey {
        case id, localPath
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        localPath = try container.decodeIfPresent(String.self, forKey: .localPath) ?? ""
    }
}

struct RemoteFileConfigCLI: Decodable {
    let pinnedFiles: [PinnedRemoteFileCLI]

    init(pinnedFiles: [PinnedRemoteFileCLI] = []) {
        self.pinnedFiles = pinnedFiles
    }
}

struct PinnedRemoteFileCLI: Decodable, Identifiable {
    let path: String

    var id: String { path }
}

struct RemoteLogConfigCLI: Decodable {
    let pinnedLogs: [PinnedRemoteLogCLI]

    init(pinnedLogs: [PinnedRemoteLogCLI] = []) {
        self.pinnedLogs = pinnedLogs
    }
}

struct PinnedRemoteLogCLI: Decodable, Identifiable {
    let path: String
    let tailLines: Int

    var id: String { path }
}

struct DatabaseConfigCLI: Decodable {
    let host: String
    let port: Int
    let name: String
    let user: String
    let useSshTunnel: Bool

    init(host: String = "localhost", port: Int = 3306, name: String = "", user: String = "", useSshTunnel: Bool = true) {
        self.host = host
        self.port = port
        self.name = name
        self.user = user
        self.useSshTunnel = useSshTunnel
    }
}

struct ToolsConfigCLI: Decodable {
    let bandcampScraper: BandcampScraperConfig?
    let newsletter: NewsletterToolConfig?

    init(bandcampScraper: BandcampScraperConfig? = nil, newsletter: NewsletterToolConfig? = nil) {
        self.bandcampScraper = bandcampScraper
        self.newsletter = newsletter
    }

    struct BandcampScraperConfig: Decodable {
        let defaultTag: String?
    }

    struct NewsletterToolConfig: Decodable {
        let sendyListId: String?
    }
}

struct ApiConfigCLI: Decodable {
    let baseUrl: String
    let enabled: Bool

    init(baseUrl: String = "", enabled: Bool = false) {
        self.baseUrl = baseUrl
        self.enabled = enabled
    }
}

// MARK: - API/Auth CLI Output Models

struct HomeboyAuthOutput: Decodable {
    let projectId: String
    let authenticated: Bool?
    let success: Bool?
}

struct HomeboyAPIGetOutput: Decodable {
    let projectId: String
    let method: String
    let endpoint: String
    let response: JSONValue
}

struct SubTargetCLI: Decodable, Identifiable {
    let name: String
    let domain: String
    let number: Int
    let isDefault: Bool

    var id: String { String(number) }
}

// MARK: - File CLI Output Models

struct FileListEntry: Decodable {
    let name: String
    let path: String
    let isDirectory: Bool
    let size: Int64?
    let permissions: String?
}

struct FileOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String?
    let oldPath: String?
    let newPath: String?
    let recursive: Bool?
    let entries: [FileListEntry]?
    let content: String?
    let bytesWritten: Int?
    let exitCode: Int32?
    let success: Bool?
}

struct LogsListEntry: Decodable {
    let path: String
    let label: String?
    let tailLines: Int?
}

struct LogsTail: Decodable {
    let path: String
    let lines: Int?
    let content: String
}

struct LogsOutput: Decodable {
    let command: String
    let projectId: String
    let entries: [LogsListEntry]?
    let log: LogsTail?
    let clearedPath: String?
    let searchResult: LogSearchResult?
}

// MARK: - File Search Types

struct FileFindOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String
    let pattern: String?
    let matches: [String]
    let matchCount: Int
}

struct FileGrepOutput: Decodable {
    let command: String
    let projectId: String
    let basePath: String?
    let path: String
    let pattern: String
    let matches: [FileGrepMatch]
    let matchCount: Int
}

struct FileGrepMatch: Decodable {
    let file: String
    let line: Int
    let content: String
}

// MARK: - Logs Search Types

struct LogSearchResult: Decodable {
    let path: String
    let pattern: String
    let matches: [LogSearchMatch]
    let matchCount: Int
}

struct LogSearchMatch: Decodable {
    let lineNumber: Int
    let content: String
}

struct DbTunnelInfo: Decodable {
    let localPort: Int
    let remoteHost: String
    let remotePort: Int
    let database: String
    let user: String
}

struct DbOutput: Decodable {
    let command: String
    let projectId: String
    let exitCode: Int32?
    let success: Bool?
    let stdout: String?
    let stderr: String?

    let tables: [String]?
    let table: String?
    let sql: String?
    let tunnel: DbTunnelInfo?
}

// MARK: - Server CLI Output Models

struct ServerOutput: Decodable {
    let command: String
    let serverId: String?
    let server: ServerRecordCLI?
    let servers: [ServerListItemCLI]?
    let updated: [String]?
    let deleted: [String]?
}

/// Server from CLI list (no id, no name in output)
struct ServerListItemCLI: Decodable, Identifiable {
    let host: String
    let port: Int
    let user: String
    let identityFile: String?

    var id: String { host }
}

/// Server from CLI show (no id, no name in output)
struct ServerRecordCLI: Decodable, Identifiable {
    let host: String
    let port: Int
    let user: String
    let identityFile: String?

    var id: String { host }
}

// MARK: - Component CLI Output Models

struct ComponentOutput: Decodable {
    let command: String
    let componentId: String?
    let component: ComponentRecordCLI?
    let components: [ComponentListItemCLI]?
    let updated: [String]?
    let deleted: [String]?
}

/// Component from CLI list (no name field)
struct ComponentListItemCLI: Decodable, Identifiable {
    let id: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
}

/// Component from CLI show (no name field)
/// Updated to match CLI's Component struct with extensions support
struct ComponentRecordCLI: Decodable, Identifiable {
    let id: String
    let aliases: [String]              // NEW: Multiple aliases for component
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
    let buildCommand: String?
    let extensions: [String: ScopedExtensionConfigCLI]?  // NEW: Extension configs by ID
    let versionTargets: [VersionTargetCLI]?
    let changelogTarget: String?       // NEW: Dedicated changelog file path
    let changelogNextSectionLabel: String?
    let changelogNextSectionAliases: [String]?
    let hooks: [String: [String]]?     // NEW: Lifecycle hooks
    let extractCommand: String?
    let remoteOwner: String?
    let deployStrategy: String?
    let gitDeploy: GitDeployConfig?
    let remoteUrl: String?
    let autoCleanup: Bool
    let docsDir: String?
    let docsDirs: [String]
    let scopes: ScopeConfig?

    struct VersionTargetCLI: Decodable {
        let file: String
        let pattern: String?
    }

    private enum CodingKeys: String, CodingKey {
        case id, aliases, localPath, remotePath, buildArtifact, buildCommand, extensions, versionTargets
        case changelogTarget, changelogNextSectionLabel, changelogNextSectionAliases, hooks
        case extractCommand, remoteOwner, deployStrategy, gitDeploy, remoteUrl, autoCleanup
        case docsDir, docsDirs, scopes
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        aliases = try container.decodeIfPresent([String].self, forKey: .aliases) ?? []
        localPath = try container.decode(String.self, forKey: .localPath)
        remotePath = try container.decode(String.self, forKey: .remotePath)
        buildArtifact = try container.decodeIfPresent(String.self, forKey: .buildArtifact)
        buildCommand = try container.decodeIfPresent(String.self, forKey: .buildCommand)
        extensions = try container.decodeIfPresent([String: ScopedExtensionConfigCLI].self, forKey: .extensions)
        versionTargets = try container.decodeIfPresent([VersionTargetCLI].self, forKey: .versionTargets)
        changelogTarget = try container.decodeIfPresent(String.self, forKey: .changelogTarget)
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
        hooks = try container.decodeIfPresent([String: [String]].self, forKey: .hooks)
        extractCommand = try container.decodeIfPresent(String.self, forKey: .extractCommand)
        remoteOwner = try container.decodeIfPresent(String.self, forKey: .remoteOwner)
        deployStrategy = try container.decodeIfPresent(String.self, forKey: .deployStrategy)
        gitDeploy = try container.decodeIfPresent(GitDeployConfig.self, forKey: .gitDeploy)
        remoteUrl = try container.decodeIfPresent(String.self, forKey: .remoteUrl)
        autoCleanup = try container.decodeIfPresent(Bool.self, forKey: .autoCleanup) ?? false
        docsDir = try container.decodeIfPresent(String.self, forKey: .docsDir)
        docsDirs = try container.decodeIfPresent([String].self, forKey: .docsDirs) ?? []
        scopes = try container.decodeIfPresent(ScopeConfig.self, forKey: .scopes)
    }
}

// MARK: - Bench CLI Output Models

struct BenchCommandOutput: Decodable {
    let passed: Bool
    let status: String
    let component: String
    let exitCode: Int32
    let iterations: Int
    let results: BenchResults?
    let baselineComparison: BenchBaselineComparison?
    let hints: [String]?
}

struct BenchResults: Decodable {
    let componentId: String
    let iterations: Int
    let scenarios: [BenchScenario]
    let metricPolicies: [String: BenchMetricPolicy]?
}

struct BenchScenario: Decodable, Identifiable {
    let id: String
    let file: String?
    let iterations: Int
    let metrics: [String: Double]
    let memory: BenchMemory?
}

struct BenchMemory: Decodable {
    let peakBytes: UInt64
}

struct BenchMetricPolicy: Decodable {
    let direction: String
    let regressionThresholdPercent: Double?
    let regressionThresholdAbsolute: Double?
}

struct BenchBaselineComparison: Decodable {
    let thresholdPercent: Double
    let scenarios: [BenchScenarioDelta]
    let newScenarioIds: [String]
    let removedScenarioIds: [String]
    let regression: Bool
    let hasImprovements: Bool
    let reasons: [String]?
}

struct BenchScenarioDelta: Decodable, Identifiable {
    let id: String
    let baselineP95Ms: Double?
    let currentP95Ms: Double?
    let p95DeltaMs: Double?
    let p95DeltaPct: Double?
    let regression: Bool
    let improvement: Bool
}

/// Extension configuration scoped to a component (for CLI output parsing)
/// Settings use [String: String] for simplicity - CLI returns settings as strings
struct ScopedExtensionConfigCLI: Decodable {
    let version: String?
    let settings: [String: String]?
}

// MARK: - Project Mutation Output Models

struct ProjectMutationOutput: Decodable {
    let command: String
    let projectId: String?
    let project: ProjectConfigCLI?
    let updated: [String]?
    let deleted: [String]?
}

// MARK: - Workspace Status Output Models

/// Output from `homeboy status --full --json` command
/// Provides full context including extensions, components, and config gaps
struct InitOutput: Decodable {
    let command: String
    let status: InitStatus
    let summary: InitSummary
    let context: ContextOutput
    let nextSteps: [String]
    let components: [ComponentSummary]
    let extensions: [ExtensionEntry]?  // NEW: Extension runtime status
    let servers: [ServerRecordCLI]
    let projects: [ProjectListItem]
    let version: VersionSnapshot?
    let git: GitSnapshot?
    let lastRelease: ReleaseSnapshot?
    let changelog: ChangelogSnapshot?
    let agentContextFiles: [String]
    let warnings: [String]
}

struct InitStatus: Decodable {
    let totalComponents: Int
    let configGaps: Int?
    let gapDetails: [ConfigGapDetail]?
    let hasUncommitted: [String]?
    let needsVersionBump: [String]?
    let readyToDeploy: [String]?
}

struct InitSummary: Decodable {
    let byExtension: [String: Int]?
    let byStatus: [String: Int]
}

struct ContextOutput: Decodable {
    let cwd: String
    let gitRoot: String?
    let managed: Bool
    let matchedComponents: [String]?
    let suggestion: String?
}

struct ComponentSummary: Decodable, Identifiable {
    let id: String
    let path: String
    let status: String
    let codeCommits: Int?
    let commitsSinceVersion: Int?
    let docsOnlyCommits: Int?
}

/// Extension entry from init output - shows runtime status
struct ExtensionEntry: Decodable, Identifiable {
    let id: String
    let name: String
    let version: String
    let description: String
    let runtime: String        // "executable" or "platform"
    let compatible: Bool       // Version compatibility
    let ready: Bool            // Runtime ready (deps installed)
    let readyReason: String?   // Why not ready (if applicable)
    let readyDetail: String?   // Detailed reason
    let linked: Bool           // Symlinked (dev) vs installed
}

struct VersionSnapshot: Decodable {
    let componentId: String?
    let version: String?
    let targets: [ComponentRecordCLI.VersionTargetCLI]?
}

struct GitSnapshot: Decodable {
    let branch: String
    let ahead: Int
    let behind: Int
    let clean: Bool
    let commitsSinceVersion: Int?
    let baselineRef: String?
}

// MARK: - Git CLI Output Models

struct GitStatusOutput: Decodable {
    let action: String
    let componentId: String
    let exitCode: Int32
    let path: String
    let stderr: String
    let stdout: String
    let success: Bool
}

struct ReleaseSnapshot: Decodable {
    let tag: String?
    let date: String?
    let summary: String?
}

struct ChangelogSnapshot: Decodable {
    let label: String?
    let path: String?
}

/// Config gap with actionable fix command
struct ConfigGapDetail: Decodable, Identifiable {
    let componentId: String
    let field: String
    let reason: String
    let command: String
    
    var id: String { "\(componentId).\(field)" }
}

@MainActor
final class HomeboyCLI {
    static let shared = HomeboyCLI()

    private let cli = CLIBridge.shared

    var isInstalled: Bool {
        cli.isInstalled
    }

private init() {}

    private static var decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private func executeCommandWithOutputFile<T: Decodable>(
        _ args: [String],
        dataType: T.Type,
        source: String,
        timeout: TimeInterval = 120
    ) async throws -> T {
        let outputURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("homeboy-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: outputURL) }

        var outputArgs = args
        outputArgs.append(contentsOf: ["--output", outputURL.path])

        let response = try await cli.execute(outputArgs, timeout: timeout)
        guard FileManager.default.fileExists(atPath: outputURL.path) else {
            if response.success {
                throw CLIBridgeError.invalidResponse("Missing output file for \(source)")
            }
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }

        let data = try Data(contentsOf: outputURL)
        let result = try Self.decoder.decode(CLIBridgeResult<T>.self, from: data)
        guard result.success else {
            if let errorDetail = result.error {
                throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
            }
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }
        guard let decoded = result.data else {
            throw CLIBridgeError.invalidResponse("Success response missing data")
        }

        return decoded
    }

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

    // MARK: - File Commands

    func fileList(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "list", projectId, path],
            dataType: FileOutput.self,
            source: "File List"
        )
    }

    func fileRead(projectId: String, path: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "read", projectId, path],
            dataType: FileOutput.self,
            source: "File Read",
            timeout: 60
        )
    }

    func fileWrite(projectId: String, path: String, content: String) async throws -> FileOutput {
        let response = try await cli.executeWithStdin(["file", "write", projectId, path], stdin: content, timeout: 60)
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
        var args = ["file", "delete", projectId, path]
        if recursive {
            args.append("--recursive")
        }
        return try await cli.executeCommand(args, dataType: FileOutput.self, source: "File Delete")
    }

    func fileRename(projectId: String, oldPath: String, newPath: String) async throws -> FileOutput {
        try await cli.executeCommand(
            ["file", "rename", projectId, oldPath, newPath],
            dataType: FileOutput.self,
            source: "File Rename"
        )
    }

    func logsList(projectId: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "list", projectId],
            dataType: LogsOutput.self,
            source: "Logs List"
        )
    }

    func logsShow(projectId: String, path: String, lines: Int?) async throws -> LogsOutput {
        var args = ["logs", "show", projectId, path]
        if let lines {
            args.append(contentsOf: ["-n", String(lines)])
        }
        return try await cli.executeCommand(args, dataType: LogsOutput.self, source: "Logs Show")
    }

    func logsClear(projectId: String, path: String) async throws -> LogsOutput {
        try await cli.executeCommand(
            ["logs", "clear", projectId, path],
            dataType: LogsOutput.self,
            source: "Logs Clear"
        )
    }

    func sshCommand(projectId: String, command: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["ssh", projectId, command], timeout: 30)
    }

    func serverKeyGenerate(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "generate", serverId], timeout: 60)
    }

    func serverKeyShow(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "show", serverId], timeout: 30)
    }

    func serverKeyUnset(serverId: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "unset", serverId], timeout: 30)
    }

    func serverKeyImport(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "import", serverId, privateKeyPath], timeout: 60)
    }

    func serverKeyUse(serverId: String, privateKeyPath: String) async throws -> CLIBridgeResponse {
        try await cli.execute(["server", "key", "use", serverId, privateKeyPath], timeout: 30)
    }

    func dbTables(projectId: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "tables", projectId],
            dataType: DbOutput.self,
            source: "Database Tables"
        )
    }

    func dbDescribe(projectId: String, table: String?) async throws -> DbOutput {
        var args = ["db", "describe", projectId]
        if let table {
            args.append(table)
        }
        return try await cli.executeCommand(args, dataType: DbOutput.self, source: "Database Describe", timeout: 60)
    }

    func dbQuery(projectId: String, sql: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "query", projectId, sql],
            dataType: DbOutput.self,
            source: "Database Query",
            timeout: 60
        )
    }

    func dbDeleteRow(projectId: String, table: String, rowId: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "delete-row", projectId, table, rowId, "--confirm"],
            dataType: DbOutput.self,
            source: "Database Delete Row"
        )
    }

    func dbDropTable(projectId: String, table: String) async throws -> DbOutput {
        try await cli.executeCommand(
            ["db", "drop-table", projectId, table, "--confirm"],
            dataType: DbOutput.self,
            source: "Database Drop Table"
        )
    }

    func dbSearch(
        projectId: String,
        table: String,
        column: String,
        pattern: String,
        exact: Bool = false,
        limit: Int? = nil,
        subtarget: String? = nil
    ) async throws -> DbOutput {
        var args = ["db", "search", projectId, table, "--column", column, "--pattern", pattern]
        if exact {
            args.append("--exact")
        }
        if let limit {
            args.append(contentsOf: ["--limit", String(limit)])
        }
        if let subtarget {
            args.append(contentsOf: ["--subtarget", subtarget])
        }
        return try await cli.executeCommand(args, dataType: DbOutput.self, source: "Database Search", timeout: 60)
    }

    // MARK: - File Search

    func fileFind(
        projectId: String,
        path: String,
        namePattern: String? = nil,
        fileType: String? = nil,
        maxDepth: Int? = nil
    ) async throws -> FileFindOutput {
        var args = ["file", "find", projectId, path]
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
        var args = ["file", "grep", projectId, path, pattern]
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

    // MARK: - Logs Search

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

    // MARK: - Quality Commands

    private enum AuditSlice {
        static let code = [
            "dead_guard",
            "stale_cli_argument_shape",
            "stale_cli_invocation",
            "unreferenced_export",
            "unused_parameter",
        ]

        static let docs = [
            "broken_doc_reference",
            "stale_doc_reference",
        ]

        static let structure = [
            "directory_sprawl",
            "duplicate_function",
            "god_file",
            "high_item_count",
            "missing_interface",
            "missing_method",
            "missing_registration",
            "naming_mismatch",
            "parallel_implementation",
            "repeated_field_pattern",
            "repeated_literal_shape",
            "shared_scaffolding",
        ]
    }

    private func audit(componentId: String, only kinds: [String], source: String, timeout: TimeInterval) async throws -> AuditOutput {
        var args = ["audit", componentId]
        for kind in kinds {
            args.append(contentsOf: ["--only", kind])
        }

        let response = try await cli.execute(args, timeout: timeout)
        let result = try response.decodeResponse(AuditOutput.self)

        if let data = result.data {
            return data
        }

        if let errorDetail = result.error {
            throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
        }
        throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
    }

    /// Run code audit on a component
    func auditCode(componentId: String, fix: Bool = false, write: Bool = false) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.code, source: "Audit Code", timeout: 120)
    }

    /// Run documentation audit on a component
    func auditDocs(componentId: String, fix: Bool = false) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.docs, source: "Audit Docs", timeout: 60)
    }

    /// Run structural audit on a component
    func auditStructure(componentId: String) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.structure, source: "Audit Structure", timeout: 60)
    }

    func qualityReviewSummary(
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> QualityReviewOutput {
        var args = ["review", componentId, "--summary"]
        appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
        return try await executeCommandWithOutputFile(args, dataType: QualityReviewOutput.self, source: "Quality Review", timeout: 180)
    }

    func qualityTriage(componentId: String) async throws -> QualityTriageOutput {
        try await executeCommandWithOutputFile(
            ["triage", "component", componentId],
            dataType: QualityTriageOutput.self,
            source: "Quality Triage",
            timeout: 60
        )
    }

    func qualityStage(
        _ stage: QualityStage,
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> CLIBridgeResponse {
        var args = [stage.rawValue, componentId]
        switch stage {
        case .audit, .lint, .test:
            appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
        case .validate:
            if let path, !path.isEmpty {
                args.append(contentsOf: ["--path", path])
            }
        }
        return try await cli.execute(args, timeout: 180)
    }

    private func appendQualityScope(
        _ args: inout [String],
        path: String?,
        scope: QualityScope,
        changedSince: String?
    ) {
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }

        switch scope {
        case .full:
            break
        case .changedSince:
            if let changedSince, !changedSince.isEmpty {
                args.append(contentsOf: ["--changed-since", changedSince])
            }
        case .changedOnly:
            args.append("--changed-only")
        }
    }

    // MARK: - Refactor Commands

    /// Generate a refactoring plan for a component
    func refactorPlan(componentId: String, from: String? = nil) async throws -> RefactorPlanOutput {
        var args = ["refactor", componentId]
        if let fromSource = from {
            args.append(contentsOf: ["--from", fromSource])
        }
        return try await cli.executeCommand(args, dataType: RefactorPlanOutput.self, source: "Refactor Plan", timeout: 60)
    }

    /// Apply a refactoring to a component
    func refactorApply(componentId: String, write: Bool = true) async throws -> RefactorResult {
        try await cli.executeCommand(
            ["refactor", componentId, "--write"],
            dataType: RefactorResult.self,
            source: "Refactor Apply",
            timeout: 120
        )
    }

    /// Decompose a large source file into smaller modules
    func refactorDecompose(componentId: String, file: String) async throws -> RefactorResult {
        try await cli.executeCommand(
            ["refactor", "decompose", componentId, file],
            dataType: RefactorResult.self,
            source: "Refactor Decompose",
            timeout: 120
        )
    }

    /// Rename a term across the codebase
    func refactorRename(componentId: String, from: String, to: String, write: Bool = false) async throws -> RefactorResult {
        var args = ["refactor", "rename", "--from", from, "--to", to, "--component", componentId]
        if write {
            args.append("--write")
        }
        return try await cli.executeCommand(args, dataType: RefactorResult.self, source: "Refactor Rename", timeout: 120)
    }

    // MARK: - Command Surface

    /// Check whether the installed CLI exposes a command or option.
    func commandSurfaceSupports(command: String, option: String? = nil) async throws -> Bool {
        var args = command.split(separator: " ").map(String.init)
        args.append("--help")

        let response = try await cli.execute(args, timeout: 10)
        guard response.success else {
            return false
        }

        guard let option else {
            return true
        }

        return response.output.contains(option) || response.errorOutput.contains(option)
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

    // MARK: - Stack Commands

    func stackList() async throws -> [StackListItem] {
        let output: StackListOutput = try await cli.executeCommand(
            ["stack", "list"],
            dataType: StackListOutput.self,
            source: "Stack List",
            timeout: 30
        )
        return output.stacks ?? []
    }

    func stackShow(id: String) async throws -> StackSpec {
        let output: StackShowOutput = try await cli.executeCommand(
            ["stack", "show", id],
            dataType: StackShowOutput.self,
            source: "Stack Show",
            timeout: 30
        )
        guard let stack = output.stack else {
            throw CLIBridgeError.invalidResponse("Stack not found: \(id)")
        }
        return stack
    }

    func stackStatus(id: String) async throws -> StackStatusOutput {
        try await cli.executeCommand(
            ["stack", "status", id],
            dataType: StackStatusOutput.self,
            source: "Stack Status",
            timeout: 120
        )
    }

    func stackInspect(path: String? = nil, base: String? = nil, includePRs: Bool = true) async throws -> StackInspectOutput {
        var args = ["stack", "inspect"]
        if let path {
            args.append(contentsOf: ["--path", path])
        }
        if let base {
            args.append(contentsOf: ["--base", base])
        }
        if !includePRs {
            args.append("--no-pr")
        }
        return try await cli.executeCommand(
            args,
            dataType: StackInspectOutput.self,
            source: "Stack Inspect",
            timeout: 120
        )
    }
}

// MARK: - New Command Output Types

/// Output from audit commands
struct AuditOutput: Decodable {
    let command: String
    let componentId: String?
    let sourcePath: String?
    let passed: Bool?
    let findings: [AuditFinding]?
    let summary: AuditSummary?
    let conventions: [AuditConvention]?
}

struct AuditFinding: Decodable, Identifiable {
    var id: String { "\(file ?? ""):\(kind):\(description)" }

    let severity: String
    let kind: String
    let convention: String?
    let confidence: String?
    let description: String
    let file: String?
    let suggestion: String?
}

struct AuditSummary: Decodable {
    let conventionsDetected: Int?
    let filesScanned: Int?
    let filesSkipped: Int?
    let outliersFound: Int?
    let warnings: [String]?
}

struct AuditConvention: Decodable {
    let name: String?
    let description: String?
    let confidence: String?
}

/// Output from refactor plan command
struct RefactorPlanOutput: Decodable {
    let command: String
    let componentId: String?
    let plan: RefactorPlan?
}

struct RefactorPlan: Decodable {
    let id: String
    let description: String
    let changes: [RefactorChange]?
    let estimatedImpact: String?
}

struct RefactorChange: Decodable, Identifiable {
    let id: String
    let type: String  // "rename", "move", "decompose", "add", etc.
    let description: String
    let files: [String]?
}

/// Output from refactor apply/decompose/rename commands
struct RefactorResult: Decodable {
    let command: String
    let componentId: String?
    let success: Bool
    let changesApplied: Int?
    let filesModified: [String]?
    let errors: [String]?
}

/// Output from `homeboy changes <component>`.
struct ChangesOutput: Decodable {
    let componentId: String?
    let baselineRef: String?
    let baselineSource: String?
    let latestTag: String?
    let path: String?
    let commits: [ChangeCommit]
    let uncommitted: UncommittedChanges?
    let changelog: ChangeChangelog?
}

struct ChangeCommit: Decodable, Identifiable {
    let hash: String
    let subject: String
    let category: String?

    var id: String { hash }
}

struct UncommittedChanges: Decodable {
    let hasChanges: Bool
    let staged: [String]?
    let unstaged: [String]?
    let untracked: [String]?
}

struct ChangeChangelog: Decodable {
    let path: String?
    let unreleasedEntries: Int?
}

/// Output from `homeboy version show <component>`.
struct VersionShowOutput: Decodable {
    let command: String?
    let componentId: String?
    let version: String?
    let path: String?
    let targets: [ComponentRecordCLI.VersionTargetCLI]?
}

/// Output from `homeboy build <component>`.
struct BuildOutput: Decodable {
    let command: String?
    let componentId: String?
    let success: Bool?
    let results: [BuildResult]?
    let summary: BuildSummary?
    let artifactPath: String?
    let message: String?
}

struct BuildResult: Decodable, Identifiable {
    let componentId: String?
    let id: String?
    let success: Bool?
    let artifactPath: String?
    let message: String?

    var stableId: String { componentId ?? id ?? artifactPath ?? message ?? UUID().uuidString }
}

struct BuildSummary: Decodable {
    let succeeded: Int?
    let failed: Int?
    let skipped: Int?
    let total: Int?
}

/// Output from `homeboy release <component> --dry-run`.
struct ReleaseOutput: Decodable {
    let command: String?
    let result: ReleaseResult?
}

struct ReleaseResult: Decodable {
    let componentId: String?
    let dryRun: Bool?
    let bumpType: String?
    let currentVersion: String?
    let nextVersion: String?
    let releasableCommits: Int?
    let skippedReason: String?
}

/// Output from undo command
struct UndoOutput: Decodable {
    let command: String
    let success: Bool
    let restoredSnapshot: UndoSnapshot?
    let message: String?
}

/// Output from undo list command
struct UndoListOutput: Decodable {
    let command: String
    let snapshots: [UndoSnapshot]?
}

struct UndoSnapshot: Decodable, Identifiable {
    let id: String
    let command: String
    let timestamp: String
    let componentId: String?
    let description: String?
}

// MARK: - Stack Output Types

struct StackListOutput: Decodable {
    let command: String
    let stacks: [StackListItem]?
}

struct StackListItem: Decodable, Identifiable, Hashable {
    let id: String
    let description: String
    let component: String
    let componentPath: String
    let base: String
    let target: String
    let prCount: Int
}

struct StackShowOutput: Decodable {
    let command: String
    let stack: StackSpec?
}

struct StackSpec: Decodable, Identifiable {
    let id: String
    let description: String
    let component: String
    let componentPath: String
    let base: StackRef
    let target: StackRef
    let prs: [StackPullRequestSpec]
}

struct StackRef: Decodable {
    let remote: String
    let branch: String

    var displayName: String { "\(remote)/\(branch)" }
}

struct StackPullRequestSpec: Decodable, Identifiable {
    let repo: String
    let number: Int
    let note: String?

    var id: String { "\(repo)#\(number)" }
}

struct StackStatusOutput: Decodable {
    let command: String
    let success: Bool?
    let stackId: String
    let componentPath: String
    let base: String
    let target: String
    let targetAhead: Int
    let targetBehind: Int
    let mergedCount: Int
    let prs: [StackPullRequestStatus]
}

struct StackPullRequestStatus: Decodable, Identifiable {
    let repo: String
    let number: Int
    let note: String?
    let title: String?
    let url: String?
    let upstreamState: String
    let localState: String
    let reviewDecision: String?

    var id: String { "\(repo)#\(number)" }
}

struct StackInspectOutput: Decodable {
    let command: String
    let success: Bool?
    let componentId: String?
    let path: String
    let branch: String
    let base: String
    let baseAutoDetected: Bool?
    let mergedCount: Int
    let commits: [StackInspectedCommit]
}

struct StackInspectedCommit: Decodable, Identifiable {
    let sha: String
    let subject: String
    let author: String?
    let date: String?
    let prNumber: Int?
    let prTitle: String?
    let prUrl: String?
    let prState: String?
    let prLookupNote: String?

    var id: String { sha }
    var shortSha: String { String(sha.prefix(8)) }
}

// MARK: - Fleet Output Types

/// Fleet struct from CLI
struct Fleet: Codable, Identifiable {
    let id: String
    let projectIds: [String]
    let description: String?
}

/// Output from fleet list command
struct FleetListOutput: Decodable {
    let command: String
    let fleets: [Fleet]?
}

/// Output from fleet show/create/delete/add/remove commands
struct FleetOutput: Decodable {
    let command: String
    let id: String?
    let fleet: Fleet?
    let message: String?
}

/// Output from fleet projects command
struct FleetProjectsOutput: Decodable {
    let command: String
    let fleetId: String?
    let projects: [ProjectListItem]?
}

/// Output from fleet components command
struct FleetComponentsOutput: Decodable {
    let command: String
    let fleetId: String?
    let components: [String: [String]]?  // component_id -> [project_id]
}

/// Output from fleet status command
struct FleetStatusOutput: Decodable {
    let command: String
    let fleetId: String
    let status: [FleetProjectStatus]
}

struct FleetProjectStatus: Decodable, Identifiable {
    let projectId: String
    let componentVersions: [String: String]?  // component_id -> version
    let health: FleetHealthStatus?

    var id: String { projectId }
}

struct FleetHealthStatus: Decodable {
    let services: [String: String]?  // service name -> status
}

/// Output from fleet check command
struct FleetCheckOutput: Decodable {
    let command: String
    let fleetId: String
    let drift: [FleetComponentDrift]?
}

struct FleetComponentDrift: Decodable, Identifiable {
    let componentId: String
    let localVersion: String
    let remoteVersions: [String: String]  // project_id -> version
    let drifted: Bool

    var id: String { componentId }
}

/// Output from fleet exec command
struct FleetExecOutput: Decodable {
    let command: String
    let fleetId: String
    let results: [FleetExecResult]
}

struct FleetExecResult: Decodable, Identifiable {
    let projectId: String
    let success: Bool
    let output: String?
    let error: String?

    var id: String { projectId }
}

// MARK: - Rig Output Types

struct RigCommandResult<T: Decodable> {
    let success: Bool
    let data: T
    let rawOutput: String
    let errorOutput: String
    let exitCode: Int32
}

struct RigListOutput: Decodable {
    let command: String
    let rigs: [RigListItem]?
}

struct RigListItem: Decodable, Identifiable {
    let id: String
    let declaredId: String?
    let description: String?
    let pipelines: [String]
    let componentCount: Int
    let serviceCount: Int
    let source: RigSource?
}

struct RigSource: Decodable {
    let source: String?
    let packagePath: String?
    let rigPath: String?
    let linked: Bool?
}

struct RigShowOutput: Decodable {
    let command: String
    let rig: RigSpec?
}

struct RigSpec: Decodable, Identifiable {
    let id: String
    let description: String?
    let components: [String: RigComponent]
    let services: [String: JSONValue]?
    let symlinks: [JSONValue]?
    let pipeline: [String: [RigStep]]?
}

struct RigComponent: Decodable, Identifiable {
    let path: String
    let branch: String?
    let stack: String?

    var id: String { path }
}

struct RigStep: Decodable, Identifiable {
    let kind: String
    let label: String?
    let idValue: String?
    let op: String?
    let component: String?
    let command: String?

    var id: String { [kind, label, idValue, op, component, command].compactMap { $0 }.joined(separator: ":") }

    enum CodingKeys: String, CodingKey {
        case kind
        case label
        case idValue = "id"
        case op
        case component
        case command
    }
}

struct RigStatusOutput: Decodable {
    let command: String
    let rigId: String
    let description: String?
    let lastUp: String?
    let lastCheck: String?
    let lastCheckResult: String?
    let services: [RigServiceStatus]
}

struct RigServiceStatus: Decodable, Identifiable {
    let id: String
    let kind: String
    let status: String
    let port: Int?
    let pid: Int?
    let startedAt: String?
    let logPath: String?
}

struct RigCheckOutput: Decodable {
    let command: String
    let rigId: String
    let success: Bool
    let pipeline: RigPipelineResult
}

struct RigLifecycleOutput: Decodable {
    let command: String
    let rigId: String?
    let success: Bool?
    let pipeline: RigPipelineResult?
}

struct RigPipelineResult: Decodable {
    let name: String
    let passed: Int?
    let failed: Int?
    let steps: [RigPipelineStep]
}

struct RigPipelineStep: Decodable, Identifiable {
    let kind: String
    let label: String
    let status: String
    let error: String?

    var id: String { "\(kind):\(label)" }
}
