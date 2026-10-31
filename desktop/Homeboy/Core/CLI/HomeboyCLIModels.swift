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
    let tools: ToolsConfigCLI
    let subTargets: [SubTargetCLI]
    let sharedTables: [String]

    private enum CodingKeys: String, CodingKey {
        case domain, serverId, basePath, tablePrefix, extensions
        case changelogNextSectionLabel, changelogNextSectionAliases
        case componentIds, components, componentOverrides, services
        case remoteFiles, tools, subTargets, sharedTables
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
        tools = try container.decodeIfPresent(ToolsConfigCLI.self, forKey: .tools) ?? ToolsConfigCLI()
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

struct ProjectPinReportOutput: Decodable {
    let command: String
    let id: String?
    let pin: ProjectPinOutput?
}

struct ProjectPinOutput: Decodable {
    let action: String
    let projectId: String
    let items: [ProjectPinListItem]?
    let added: ProjectPinChange?
    let removed: ProjectPinChange?
}

struct ProjectPinListItem: Decodable {
    let path: String
    let label: String?
    let displayName: String
}

struct ProjectPinChange: Decodable {
    let path: String
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

struct RunsArtifactSyncOutput: Decodable {
    let command: String
    let runId: String
    let artifacts: [RunArtifactSyncItem]
}

struct RunArtifactSyncItem: Decodable, Identifiable {
    let id: String
    let pathToken: String
    let runId: String
    let kind: String
    let artifactType: String
    let downloadPath: String
    let sha256: String?
    let sizeBytes: Int64?
    let mime: String?
    let createdAt: String

    private enum CodingKeys: String, CodingKey {
        case id, pathToken, runId, kind, downloadPath, sha256, sizeBytes, mime, createdAt
        case artifactType = "type"
    }
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

// MARK: - Homeboy Lab Runner Models

struct RunnerListOutput: Decodable {
    let command: String
    let entities: [HomeboyRunner]
}

struct RunnerShowOutput: Decodable {
    let command: String
    let id: String?
    let entity: HomeboyRunner?
}

struct RunnerConnectionOutput: Decodable {
    let command: String
    let id: String?
    let extra: RunnerExtra?
}

struct RunnerExtra: Decodable {
    let connection: RunnerConnection?
}

struct RunnerConnection: Decodable {
    let action: String
    let runnerId: String
    let connected: Bool?
    let disconnected: Bool?
    let localUrl: String?
    let remoteDaemonAddress: String?
    let tunnelPid: Int?
    let remoteDaemonPid: Int?
    let homeboyVersion: String?
    let sessionPath: String?
    let failureKind: String?
    let failureMessage: String?
    let session: RunnerSession?
}

struct RunnerSession: Decodable {
    let runnerId: String
    let serverId: String?
    let remoteDaemonAddress: String
    let localPort: Int
    let localUrl: String
    let tunnelPid: Int?
    let remoteDaemonPid: Int?
    let homeboyVersion: String
    let connectedAt: String
}

struct HomeboyRunner: Decodable, Identifiable {
    let id: String
    let kind: String
    let serverId: String?
    let workspaceRoot: String?
    let homeboyPath: String?
    let daemon: Bool
    let concurrencyLimit: Int?
    let artifactPolicy: String?
    let env: [String: JSONValue]
    let resources: [String: JSONValue]

    static let localDefault = HomeboyRunner(
        id: "local",
        kind: "local",
        serverId: nil,
        workspaceRoot: nil,
        homeboyPath: nil,
        daemon: false,
        concurrencyLimit: nil,
        artifactPolicy: nil,
        env: [:],
        resources: [:]
    )
}

struct RunnerDoctorOutput: Decodable {
    let command: String
    let runnerId: String
    let runner: RunnerTargetSummary
    let status: String
    let capabilities: RunnerCapabilities
    let resources: RunnerResources
    let checks: [RunnerCheck]
}

struct RunnerTargetSummary: Decodable {
    let targetType: String
    let registry: RunnerRegistrySummary?
    let server: RunnerServerSummary?

    private enum CodingKeys: String, CodingKey {
        case targetType = "type"
        case registry
        case server
    }
}

struct RunnerRegistrySummary: Decodable {
    let id: String
    let kind: String
}

struct RunnerServerSummary: Decodable {
    let id: String
    let host: String
    let user: String
    let port: Int
    let isLocalhost: Bool
}

struct RunnerCapabilities: Decodable {
    let localExecution: Bool
    let sshExecution: Bool
    let git: Bool
    let githubCli: Bool
    let node: Bool
    let npm: Bool
    let pnpm: Bool
    let php: Bool
    let composer: Bool
    let docker: Bool
    let playwright: Bool
    let browserReady: Bool
    let workspaceWritable: Bool
    let artifactStoreAvailable: Bool
}

struct RunnerResources: Decodable {
    let homeboy: RunnerHomeboyProbe
    let system: RunnerSystemProbe
    let cpu: RunnerCPUProbe
    let memory: RunnerMemoryProbe?
    let disk: RunnerDiskProbe?
    let workspaceRoot: String
    let artifactRoot: String
    let tools: [String: RunnerToolProbe]
}

struct RunnerHomeboyProbe: Decodable {
    let version: String
    let path: String?
}

struct RunnerSystemProbe: Decodable {
    let os: String
    let arch: String
    let kernel: String?
}

struct RunnerCPUProbe: Decodable {
    let count: Int
}

struct RunnerMemoryProbe: Decodable {
    let totalMb: Int64
    let availableMb: Int64?
}

struct RunnerDiskProbe: Decodable {
    let path: String
    let totalMb: Int64
    let availableMb: Int64
}

struct RunnerToolProbe: Decodable {
    let available: Bool
    let path: String?
    let version: String?
    let error: String?
}

struct RunnerCheck: Decodable, Identifiable {
    let id: String
    let status: String
    let message: String
    let remediation: String?
    let details: [String: String]
}

struct DaemonEnvelope<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
}

struct DaemonAPIResponse<T: Decodable>: Decodable {
    let status: Int
    let endpoint: String
    let body: T
}

struct DaemonJobsOutput: Decodable {
    let command: String
    let jobs: [DaemonJob]
}

struct DaemonJobOutput: Decodable {
    let command: String
    let job: DaemonJob
}

struct DaemonJobEventsOutput: Decodable {
    let command: String
    let jobId: String
    let events: [DaemonJobEvent]
}

struct DaemonJobEnqueueOutput: Decodable {
    let command: String
    let job: DaemonJob
    let poll: DaemonJobPoll
    let request: JSONValue
}

struct DaemonJobPoll: Decodable {
    let job: String
    let events: String
}

struct DaemonJob: Decodable, Identifiable {
    let id: String
    let operation: String
    let status: String
    let createdAtMs: UInt64
    let updatedAtMs: UInt64
    let startedAtMs: UInt64?
    let finishedAtMs: UInt64?
    let eventCount: Int
    let staleReason: String?
}

struct DaemonJobEvent: Decodable, Identifiable {
    let sequence: UInt64
    let jobId: String
    let kind: String
    let timestampMs: UInt64
    let message: String?
    let data: JSONValue?

    var id: String { "\(jobId):\(sequence)" }
}
