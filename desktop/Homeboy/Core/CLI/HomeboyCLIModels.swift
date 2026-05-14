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

    private enum CodingKeys: String, CodingKey {
        case command, project, projectId, entity, id
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        command = try container.decode(String.self, forKey: .command)
        project = try container.decodeIfPresent(ProjectConfigCLI.self, forKey: .project)
            ?? container.decodeIfPresent(ProjectConfigCLI.self, forKey: .entity)
        projectId = try container.decodeIfPresent(String.self, forKey: .projectId)
            ?? container.decodeIfPresent(String.self, forKey: .id)
    }
}

/// Project configuration matching CLI's Project struct (no wrapper)
struct ProjectConfigCLI: Decodable {
    let domain: String?
    let serverId: String?
    let basePath: String?
    let tablePrefix: String?
    let extensions: [String: JSONValue]
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
        case domain, serverId, basePath, tablePrefix, extensions
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
        extensions = try container.decodeIfPresent([String: JSONValue].self, forKey: .extensions) ?? [:]
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
    let size: Int64?
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

struct ProjectPinReportOutput: Decodable {
    let command: String
    let id: String?
    let pin: ProjectPinOutput?
}

struct ProjectPinOutput: Decodable {
    let action: String
    let projectId: String
    let type: String
    let items: [ProjectPinListItem]?
    let added: ProjectPinChange?
    let removed: ProjectPinChange?
}

struct ProjectPinListItem: Decodable {
    let path: String
    let label: String?
    let displayName: String
    let tailLines: Int?
}

struct ProjectPinChange: Decodable {
    let path: String
    let type: String
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

// MARK: - Fleet CLI Output Models

struct Fleet: Decodable, Identifiable, Hashable {
    let id: String
    let projectIds: [String]
    let description: String?
    let componentOverrides: [String: ProjectComponentOverrides]
    let priorityLabels: [String]?

    private enum CodingKeys: String, CodingKey {
        case id, projectIds, description, componentOverrides, priorityLabels
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        projectIds = try container.decodeIfPresent([String].self, forKey: .projectIds) ?? []
        description = try container.decodeIfPresent(String.self, forKey: .description)
        componentOverrides = try container.decodeIfPresent([String: ProjectComponentOverrides].self, forKey: .componentOverrides) ?? [:]
        priorityLabels = try container.decodeIfPresent([String].self, forKey: .priorityLabels)
    }

    static func == (lhs: Fleet, rhs: Fleet) -> Bool {
        lhs.id == rhs.id
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }
}

struct FleetOutput: Decodable {
    let command: String
    let id: String?
    let entity: Fleet?
    let entities: [Fleet]?
    let updatedFields: [String]?
    let deleted: [String]?

    var fleet: Fleet? { entity }
    var fleets: [Fleet]? { entities }
}

typealias FleetListOutput = FleetOutput

struct FleetProjectsOutput: Decodable {
    let command: String
    let id: String?
    let projects: [ProjectListItem]?
}

struct FleetComponentsOutput: Decodable {
    let command: String
    let id: String?
    let components: [String: [String]]?
}

struct FleetStatusOutput: Decodable {
    let command: String
    let id: String?
    let status: FleetStatusResult?
}

struct FleetStatusResult: Decodable {
    let projects: [FleetProjectStatus]
    let summary: FleetStatusSummary
}

struct FleetProjectStatus: Decodable, Identifiable {
    let projectId: String
    let serverId: String?
    let components: [FleetComponentStatus]
    let health: JSONValue?

    var id: String { projectId }
}

struct FleetComponentStatus: Decodable, Identifiable {
    let componentId: String
    let localVersion: String?
    let remoteVersion: String?
    let versionSource: String
    let drift: String
    let unreleasedCommits: UInt32

    var id: String { componentId }
}

struct FleetStatusSummary: Decodable {
    let projects: FleetProjectSummary
    let components: FleetComponentSummary
    let servers: FleetServerSummary
    let warnings: [FleetWarning]?
}

struct FleetProjectSummary: Decodable {
    let total: UInt32
    let healthy: UInt32
    let warning: UInt32
    let unreachable: UInt32
}

struct FleetComponentSummary: Decodable {
    let total: UInt32
    let current: UInt32
    let needsUpdate: UInt32
    let needsRelease: UInt32
    let docsOnly: UInt32
    let unknown: UInt32
}

struct FleetServerSummary: Decodable {
    let total: UInt32
    let healthy: UInt32
    let warning: UInt32
    let unreachable: UInt32
    let servicesUp: UInt32
    let servicesDown: UInt32
}

struct FleetWarning: Decodable, Identifiable {
    let serverId: String
    let projectId: String
    let message: String

    var id: String { "\(serverId):\(projectId):\(message)" }
}

struct FleetCheckOutput: Decodable {
    let command: String
    let id: String?
    let check: [FleetProjectCheck]?
    let summary: FleetCheckSummary?
}

struct FleetProjectCheck: Decodable, Identifiable {
    let projectId: String
    let serverId: String?
    let status: String
    let error: String?
    let components: [FleetComponentCheck]

    var id: String { projectId }
}

struct FleetComponentCheck: Decodable, Identifiable {
    let componentId: String
    let localVersion: String?
    let remoteVersion: String?
    let status: String

    var id: String { componentId }
}

struct FleetCheckSummary: Decodable {
    let totalProjects: UInt32
    let projectsChecked: UInt32
    let projectsFailed: UInt32
    let componentsUpToDate: UInt32
    let componentsNeedsUpdate: UInt32
    let componentsUnknown: UInt32
}

struct FleetExecOutput: Decodable {
    let command: String
    let id: String?
    let exec: [FleetExecProjectResult]?
    let execSummary: FleetExecSummary?
}

struct FleetExecProjectResult: Decodable, Identifiable {
    let projectId: String
    let serverId: String?
    let basePath: String?
    let command: String
    let status: String
    let stdout: String?
    let stderr: String?
    let exitCode: Int32?
    let error: String?

    var id: String { projectId }
}

struct FleetExecSummary: Decodable {
    let total: UInt32
    let succeeded: UInt32
    let failed: UInt32
    let skipped: UInt32
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

// MARK: - Rig CLI Output Models

struct RigListOutput: Decodable {
    let command: String
    let rigs: [RigListItem]?
}

struct RigListItem: Decodable, Identifiable {
    let id: String
    let declaredId: String?
    let description: String?
    let componentCount: Int
    let serviceCount: Int
    let pipelines: [String]
    let source: RigSourceSummary?
}

struct RigSourceSummary: Decodable {
    let source: String
    let packagePath: String
    let rigPath: String
    let linked: Bool
    let sourceRevision: String?
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
    let pipeline: [String: [RigPipelineStepSpec]]?
}

struct RigComponent: Decodable {
    let path: String
    let branch: String?
}

struct RigPipelineStepSpec: Decodable {
    let kind: String
    let label: String?
}

struct RigStatusOutput: Decodable {
    let command: String
    let rigId: String
    let description: String
    let services: [RigServiceStatus]
    let symlinks: [JSONValue]
    let lastUp: String?
    let lastCheck: String?
    let lastCheckResult: String?
    let materialized: JSONValue?
}

struct RigServiceStatus: Decodable, Identifiable {
    let id: String
    let kind: String
    let status: String
    let pid: UInt32?
    let port: UInt16?
    let logPath: String
    let startedAt: String?
}

struct RigCommandResult<T: Decodable> {
    let success: Bool
    let data: T
    let rawOutput: String
    let errorOutput: String
    let exitCode: Int32
}

struct RigCheckOutput: Decodable {
    let command: String
    let rigId: String
    let pipeline: RigPipelineResult
    let success: Bool
}

struct RigLifecycleOutput: Decodable {
    let command: String
    let rigId: String
    let pipeline: RigPipelineResult?
    let stopped: [String]?
    let success: Bool
}

struct RigPipelineResult: Decodable {
    let name: String
    let steps: [RigPipelineStep]
    let passed: Int?
    let failed: Int?
}

struct RigPipelineStep: Decodable, Identifiable {
    let kind: String
    let label: String
    let status: String
    let error: String?

    var id: String { "\(kind):\(label)" }
}

// MARK: - Release / Build CLI Output Models

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

struct VersionShowOutput: Decodable {
    let command: String?
    let componentId: String?
    let version: String?
    let path: String?
    let targets: [ComponentRecordCLI.VersionTargetCLI]?
}

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

struct BuildOutput: Decodable {
    let command: String
    let componentId: String
    let buildCommand: String
    let stdout: String?
    let stderr: String?
    let success: Bool
}

// MARK: - Stack CLI Output Models

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
    let base: StackGitRef
    let target: StackGitRef
    let prs: [StackPrEntry]
}

struct StackGitRef: Decodable {
    let remote: String
    let branch: String
}

struct StackPrEntry: Decodable, Identifiable {
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

    private enum CodingKeys: String, CodingKey {
        case command, success, stackId, componentPath, base, target, targetAhead, targetBehind, mergedCount, prs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        command = try container.decode(String.self, forKey: .command)
        success = try container.decodeIfPresent(Bool.self, forKey: .success)
        stackId = try container.decode(String.self, forKey: .stackId)
        componentPath = try container.decode(String.self, forKey: .componentPath)
        base = try container.decode(String.self, forKey: .base)
        target = try container.decode(String.self, forKey: .target)
        targetAhead = try container.decodeIfPresent(Int.self, forKey: .targetAhead) ?? 0
        targetBehind = try container.decodeIfPresent(Int.self, forKey: .targetBehind) ?? 0
        mergedCount = try container.decode(Int.self, forKey: .mergedCount)
        prs = try container.decode([StackPullRequestStatus].self, forKey: .prs)
    }
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
    let mergedAt: String?
    let candidateForDrop: Bool
    let error: String?

    var id: String { "\(repo)#\(number)" }

    private enum CodingKeys: String, CodingKey {
        case repo, number, note, title, url, upstreamState, localState, reviewDecision, mergedAt, candidateForDrop, error
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        repo = try container.decode(String.self, forKey: .repo)
        number = try container.decode(Int.self, forKey: .number)
        note = try container.decodeIfPresent(String.self, forKey: .note)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        url = try container.decodeIfPresent(String.self, forKey: .url)
        upstreamState = try container.decodeIfPresent(String.self, forKey: .upstreamState) ?? "UNKNOWN"
        localState = try container.decodeIfPresent(String.self, forKey: .localState) ?? "unknown"
        reviewDecision = try container.decodeIfPresent(String.self, forKey: .reviewDecision)
        mergedAt = try container.decodeIfPresent(String.self, forKey: .mergedAt)
        candidateForDrop = try container.decodeIfPresent(Bool.self, forKey: .candidateForDrop) ?? false
        error = try container.decodeIfPresent(String.self, forKey: .error)
    }
}

struct StackInspectOutput: Decodable {
    let command: String
    let componentId: String
    let path: String
    let branch: String
    let base: String
    let baseAutoDetected: Bool
    let commits: [StackInspectCommit]
    let mergedCount: Int
    let success: Bool
}

struct StackInspectCommit: Decodable, Identifiable {
    let sha: String
    let shortSha: String
    let subject: String
    let author: String
    let date: String
    let pr: StackInspectPullRequest?
    let prLookupNote: String?

    var id: String { sha }
}

struct StackInspectPullRequest: Decodable {
    let number: Int
    let state: String
    let title: String
    let url: String
}

// MARK: - Undo CLI Output Models

struct UndoOutput: Decodable {
    let command: String
    let snapshotId: String?
    let label: String?
    let filesRestored: Int?
    let filesRemoved: Int?
    let errors: [String]?
    let id: String?
    let deleted: Bool?
}

struct UndoListOutput: Decodable {
    let command: String
    let snapshots: [UndoSnapshot]?
}

struct UndoSnapshot: Decodable, Identifiable {
    let id: String
    let label: String
    let root: String
    let fileCount: Int
    let createdAt: UInt64
    let age: String
}

// MARK: - Run History CLI Output Models

struct RunsListOutput: Decodable {
    let command: String
    let runs: [RunSummary]
}

struct RunsShowOutput: Decodable {
    let command: String
    let run: RunDetail
}

struct RunsArtifactsOutput: Decodable {
    let command: String
    let runId: String
    let artifacts: [RunArtifact]
}

struct RunsFindingsOutput: Decodable {
    let command: String
    let runId: String
    let findings: [RunFinding]
}

struct RunSummary: Decodable, Identifiable {
    let id: String
    let kind: String
    let status: String
    let startedAt: String
    let finishedAt: String?
    let componentId: String?
    let rigId: String?
    let gitSha: String?
    let command: String?
    let cwd: String?
    let statusNote: String?
}

struct RunDetail: Decodable, Identifiable {
    let id: String
    let kind: String
    let status: String
    let startedAt: String
    let finishedAt: String?
    let componentId: String?
    let rigId: String?
    let gitSha: String?
    let command: String?
    let cwd: String?
    let statusNote: String?
    let homeboyVersion: String?
    let metadata: JSONValue
    let artifacts: [RunArtifact]
}

struct RunArtifact: Decodable, Identifiable {
    let id: String
    let runId: String
    let kind: String
    let artifactType: String
    let path: String
    let url: String?
    let sha256: String?
    let sizeBytes: Int64?
    let mime: String?
    let createdAt: String

    private enum CodingKeys: String, CodingKey {
        case id, runId, kind, path, url, sha256, sizeBytes, mime, createdAt
        case artifactType = "type"
    }
}

struct RunFinding: Decodable, Identifiable {
    let id: String
    let runId: String
    let tool: String
    let rule: String?
    let file: String?
    let line: Int64?
    let severity: String?
    let fingerprint: String?
    let message: String
    let fixable: Bool?
    let metadataJson: JSONValue
    let createdAt: String
}

struct RunsCompareOutput: Decodable {
    let command: String
    let kind: String
    let componentId: String?
    let rigId: String?
    let scenarioId: String?
    let metrics: [String]
    let rows: [RunsCompareRow]
}

struct RunsCompareRow: Decodable, Identifiable {
    let run: RunSummary
    let artifactCount: Int
    let scenarioId: String?
    let metrics: [String: Double?]

    var id: String {
        [run.id, scenarioId].compactMap { $0 }.joined(separator: ":")
    }
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
