import Foundation

// MARK: - CLI Response Types (mirror DeployerViewModel.swift)

struct CLIResponse<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
    let error: CLIErrorResponse?
}

struct CLIErrorResponse: Decodable {
    let code: String
    let message: String
}

struct CLIDeploymentResult: Decodable {
    let results: [CLIComponentResult]
    let summary: CLIDeploymentSummary
}

struct CLIComponentResult: Decodable {
    let id: String
    let status: String
    let localVersion: String?
    let remoteVersion: String?
    let componentStatus: String?
    let error: String?
    let artifactPath: String?
    let remotePath: String?
}

struct CLIDeploymentSummary: Decodable {
    let succeeded: Int
    let failed: Int
    let skipped: Int
    let total: Int
}

// MARK: - Component List Types (mirror HomeboyCLI.swift)

struct ComponentListOutput: Decodable {
    let command: String
    let entities: [ComponentListItemCLI]?
}

struct ComponentListItemCLI: Decodable {
    let id: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
}

// MARK: - Extension List Types (mirror CLIExtensionTypes.swift)

struct ExtensionListOutput: Decodable {
    let command: String?
    let projectId: String?
    let extensions: [ExtensionListItemCLI]
}

struct ExtensionListItemCLI: Decodable {
    let id: String
    let name: String
    let version: String
    let description: String
    let runtime: String
    let compatible: Bool
    let ready: Bool
    let configured: Bool?
    let linked: Bool
    let path: String
    let actions: [ExtensionActionCLI]?
    let hasReadyCheck: Bool?
    let hasSetup: Bool?
    let cliDisplayName: String?
    let cliTool: String?
    let sourceRevision: String?
}

struct ExtensionActionCLI: Decodable {
    let id: String
    let label: String
    let type: String
}

// MARK: - Database Output Types (mirror HomeboyCLI.swift)

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
}

// MARK: - Stack Output Types (mirror HomeboyCLI.swift)

struct StackListOutput: Decodable {
    let command: String
    let stacks: [StackListItem]?
}

struct StackListItem: Decodable {
    let id: String
    let description: String
    let component: String
    let componentPath: String
    let base: String
    let target: String
    let prCount: Int
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

struct StackPullRequestStatus: Decodable {
    let repo: String
    let number: Int
    let note: String?
    let title: String?
    let url: String?
    let upstreamState: String
    let localState: String
    let reviewDecision: String?
}

// MARK: - Rig Output Types (mirror HomeboyCLI.swift)

enum JSONValueTest: Decodable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case null
    case array([JSONValueTest])
    case object([String: JSONValueTest])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(Double.self) {
            self = .double(value)
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode([JSONValueTest].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: JSONValueTest].self) {
            self = .object(value)
        } else {
            throw DecodingError.typeMismatch(JSONValueTest.self, DecodingError.Context(codingPath: decoder.codingPath, debugDescription: "Unable to decode JSON value"))
        }
    }
}

struct RigListOutputTest: Decodable {
    let command: String
    let rigs: [RigListItemTest]?
}

struct RigListItemTest: Decodable {
    let id: String
    let description: String?
    let pipelines: [String]
    let componentCount: Int
    let serviceCount: Int
}

struct RigShowOutputTest: Decodable {
    let command: String
    let rig: RigSpecTest?
}

struct RigSpecTest: Decodable {
    let id: String
    let description: String?
    let components: [String: RigComponentTest]
    let services: [String: JSONValueTest]?
    let symlinks: [JSONValueTest]?
    let pipeline: [String: [RigStepTest]]?
}

struct RigComponentTest: Decodable {
    let path: String
    let branch: String?
}

struct RigStepTest: Decodable {
    let kind: String
    let label: String?
}

struct RigCheckOutputTest: Decodable {
    let command: String
    let rigId: String
    let success: Bool
    let pipeline: RigPipelineResultTest
}

struct RigPipelineResultTest: Decodable {
    let name: String
    let passed: Int?
    let failed: Int?
    let steps: [RigPipelineStepTest]
}

struct RigPipelineStepTest: Decodable {
    let kind: String
    let label: String
    let status: String
    let error: String?
}

// MARK: - API/Auth Output Types (mirror HomeboyCLI.swift)

struct HomeboyAuthOutputTest: Decodable {
    let projectId: String
    let authenticated: Bool?
    let success: Bool?
}

struct HomeboyAPIGetOutputTest: Decodable {
    let projectId: String
    let method: String
    let endpoint: String
    let response: JSONValueTest
}

struct WPTable: Decodable {
    let Name: String
    let Rows: String?
    let Engine: String?
}

// MARK: - Component Configuration Types (mirror ComponentConfiguration.swift)

struct VersionTargetTest: Decodable {
    let file: String
    let pattern: String?
}

struct CommandScopeConfigTest: Decodable {
    let include: [String]
    let exclude: [String]
}

struct ScopeConfigTest: Decodable {
    let defaults: CommandScopeConfigTest?
    let audit: CommandScopeConfigTest?
    let lint: CommandScopeConfigTest?
    let test: CommandScopeConfigTest?
    let refactor: CommandScopeConfigTest?
    let deploy: CommandScopeConfigTest?
    let release: CommandScopeConfigTest?
    let fleet: CommandScopeConfigTest?
}

struct GitDeployConfigTest: Decodable {
    let remote: String
    let branch: String
    let postPull: [String]
    let tagPattern: String?

    private enum CodingKeys: String, CodingKey {
        case remote, branch, postPull, tagPattern
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        remote = try container.decodeIfPresent(String.self, forKey: .remote) ?? "origin"
        branch = try container.decodeIfPresent(String.self, forKey: .branch) ?? "main"
        postPull = try container.decodeIfPresent([String].self, forKey: .postPull) ?? []
        tagPattern = try container.decodeIfPresent(String.self, forKey: .tagPattern)
    }
}

struct ComponentConfigurationTest: Decodable {
    let id: String
    let aliases: [String]
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
    let versionTargets: [VersionTargetTest]?
    let buildCommand: String?
    let changelogNextSectionLabel: String?
    let changelogNextSectionAliases: [String]?
    let extractCommand: String?
    let remoteOwner: String?
    let deployStrategy: String?
    let gitDeploy: GitDeployConfigTest?
    let remoteUrl: String?
    let autoCleanup: Bool
    let docsDir: String?
    let docsDirs: [String]
    let scopes: ScopeConfigTest?

    private enum CodingKeys: String, CodingKey {
        case id, aliases, localPath, remotePath, buildArtifact, versionTargets, buildCommand
        case changelogNextSectionLabel, changelogNextSectionAliases, extractCommand, remoteOwner
        case deployStrategy, gitDeploy, remoteUrl, autoCleanup, docsDir, docsDirs, scopes
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        aliases = try container.decodeIfPresent([String].self, forKey: .aliases) ?? []
        localPath = try container.decode(String.self, forKey: .localPath)
        remotePath = try container.decode(String.self, forKey: .remotePath)
        buildArtifact = try container.decodeIfPresent(String.self, forKey: .buildArtifact)
        versionTargets = try container.decodeIfPresent([VersionTargetTest].self, forKey: .versionTargets)
        buildCommand = try container.decodeIfPresent(String.self, forKey: .buildCommand)
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
        extractCommand = try container.decodeIfPresent(String.self, forKey: .extractCommand)
        remoteOwner = try container.decodeIfPresent(String.self, forKey: .remoteOwner)
        deployStrategy = try container.decodeIfPresent(String.self, forKey: .deployStrategy)
        gitDeploy = try container.decodeIfPresent(GitDeployConfigTest.self, forKey: .gitDeploy)
        remoteUrl = try container.decodeIfPresent(String.self, forKey: .remoteUrl)
        autoCleanup = try container.decodeIfPresent(Bool.self, forKey: .autoCleanup) ?? false
        docsDir = try container.decodeIfPresent(String.self, forKey: .docsDir)
        docsDirs = try container.decodeIfPresent([String].self, forKey: .docsDirs) ?? []
        scopes = try container.decodeIfPresent(ScopeConfigTest.self, forKey: .scopes)
    }

    var displayName: String {
        id.split(separator: "-")
            .map { $0.capitalized }
            .joined(separator: " ")
    }

    var versionFile: String? {
        versionTargets?.first?.file
    }

    var versionPattern: String? {
        versionTargets?.first?.pattern
    }
}

struct ProjectComponentAttachmentTest: Decodable {
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

struct ProjectComponentOverridesTest: Decodable {
    let remotePath: String?
    let buildArtifact: String?
    let extractCommand: String?
    let remoteOwner: String?
    let deployStrategy: String?
    let gitDeploy: GitDeployConfigTest?
    let hooks: [String: [String]]
    let scopes: ScopeConfigTest?
    let cliPath: String?

    private enum CodingKeys: String, CodingKey {
        case remotePath, buildArtifact, extractCommand, remoteOwner, deployStrategy
        case gitDeploy, hooks, scopes, cliPath
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        remotePath = try container.decodeIfPresent(String.self, forKey: .remotePath)
        buildArtifact = try container.decodeIfPresent(String.self, forKey: .buildArtifact)
        extractCommand = try container.decodeIfPresent(String.self, forKey: .extractCommand)
        remoteOwner = try container.decodeIfPresent(String.self, forKey: .remoteOwner)
        deployStrategy = try container.decodeIfPresent(String.self, forKey: .deployStrategy)
        gitDeploy = try container.decodeIfPresent(GitDeployConfigTest.self, forKey: .gitDeploy)
        hooks = try container.decodeIfPresent([String: [String]].self, forKey: .hooks) ?? [:]
        scopes = try container.decodeIfPresent(ScopeConfigTest.self, forKey: .scopes)
        cliPath = try container.decodeIfPresent(String.self, forKey: .cliPath)
    }
}

struct ProjectConfigCLITest: Decodable {
    let domain: String?
    let changelogNextSectionLabel: String?
    let changelogNextSectionAliases: [String]?
    let components: [ProjectComponentAttachmentTest]
    let componentIds: [String]
    let componentOverrides: [String: ProjectComponentOverridesTest]
    let services: [String]

    private enum CodingKeys: String, CodingKey {
        case domain, changelogNextSectionLabel, changelogNextSectionAliases
        case components, componentIds, componentOverrides, services
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        domain = try container.decodeIfPresent(String.self, forKey: .domain)
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
        components = try container.decodeIfPresent([ProjectComponentAttachmentTest].self, forKey: .components) ?? []
        componentIds = try container.decodeIfPresent([String].self, forKey: .componentIds) ?? components.map { $0.id }
        componentOverrides = try container.decodeIfPresent([String: ProjectComponentOverridesTest].self, forKey: .componentOverrides) ?? [:]
        services = try container.decodeIfPresent([String].self, forKey: .services) ?? []
    }
}

// MARK: - Bench Output Types (mirror HomeboyCLI.swift)

struct BenchCommandOutputTest: Decodable {
    let passed: Bool
    let status: String
    let component: String
    let exitCode: Int32
    let iterations: Int
    let results: BenchResultsTest?
    let baselineComparison: BenchBaselineComparisonTest?
    let hints: [String]?
}

struct BenchResultsTest: Decodable {
    let componentId: String
    let iterations: Int
    let scenarios: [BenchScenarioTest]
    let metricPolicies: [String: BenchMetricPolicyTest]?
}

struct BenchScenarioTest: Decodable {
    let id: String
    let file: String?
    let iterations: Int
    let metrics: [String: Double]
    let memory: BenchMemoryTest?
}

struct BenchMemoryTest: Decodable {
    let peakBytes: UInt64
}

struct BenchMetricPolicyTest: Decodable {
    let direction: String
    let regressionThresholdPercent: Double?
    let regressionThresholdAbsolute: Double?
}

struct BenchBaselineComparisonTest: Decodable {
    let thresholdPercent: Double
    let scenarios: [BenchScenarioDeltaTest]
    let newScenarioIds: [String]
    let removedScenarioIds: [String]
    let regression: Bool
    let hasImprovements: Bool
    let reasons: [String]?
}

struct BenchScenarioDeltaTest: Decodable {
    let id: String
    let baselineP95Ms: Double?
    let currentP95Ms: Double?
    let p95DeltaMs: Double?
    let p95DeltaPct: Double?
    let regression: Bool
    let improvement: Bool
}

// MARK: - Run History Output Types (mirror HomeboyCLI.swift)

struct RunsListOutputTest: Decodable {
    let command: String
    let runs: [RunSummaryTest]
}

struct RunsShowOutputTest: Decodable {
    let command: String
    let run: RunDetailTest
}

struct RunsArtifactsOutputTest: Decodable {
    let command: String
    let runId: String
    let artifacts: [RunArtifactTest]
}

struct RunsFindingsOutputTest: Decodable {
    let command: String
    let runId: String
    let findings: [RunFindingTest]
}

struct RunSummaryTest: Decodable {
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

struct RunDetailTest: Decodable {
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
    let metadata: JSONValueTest
    let artifacts: [RunArtifactTest]
}

struct RunArtifactTest: Decodable {
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

struct RunFindingTest: Decodable {
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
    let metadataJson: JSONValueTest
    let createdAt: String
}

struct RunsCompareOutputTest: Decodable {
    let command: String
    let kind: String
    let componentId: String?
    let rigId: String?
    let scenarioId: String?
    let metrics: [String]
    let rows: [RunsCompareRowTest]
}

struct RunsCompareRowTest: Decodable {
    let run: RunSummaryTest
    let artifactCount: Int
    let scenarioId: String?
    let metrics: [String: Double?]
}

// MARK: - Release / Build Output Types (mirror HomeboyCLI.swift)

struct ChangesOutputTest: Decodable {
    let componentId: String?
    let baselineRef: String?
    let baselineSource: String?
    let latestTag: String?
    let path: String?
    let commits: [ChangeCommitTest]
    let uncommitted: UncommittedChangesTest?
    let changelog: ChangeChangelogTest?
}

struct ChangeCommitTest: Decodable {
    let hash: String
    let subject: String
    let category: String?
}

struct UncommittedChangesTest: Decodable {
    let hasChanges: Bool
    let staged: [String]?
    let unstaged: [String]?
    let untracked: [String]?
}

struct ChangeChangelogTest: Decodable {
    let path: String?
    let unreleasedEntries: Int?
}

struct VersionShowOutputTest: Decodable {
    let command: String?
    let componentId: String?
    let version: String?
    let path: String?
    let targets: [VersionTargetTest]?
}

struct ReleaseOutputTest: Decodable {
    let command: String?
    let result: ReleaseResultTest?
}

struct ReleaseResultTest: Decodable {
    let componentId: String?
    let dryRun: Bool?
    let bumpType: String?
    let currentVersion: String?
    let nextVersion: String?
    let releasableCommits: Int?
    let skippedReason: String?
}

// MARK: - Git Output Types (mirror HomeboyCLI.swift)

struct GitStatusOutputTest: Decodable {
    let action: String
    let componentId: String
    let exitCode: Int32
    let path: String
    let stderr: String
    let stdout: String
    let success: Bool
}

// MARK: - Shared Helpers

func sourceSlice(_ source: String, from startMarker: String, to endMarker: String) throws -> String {
    guard let start = source.range(of: startMarker)?.lowerBound else {
        throw NSError(domain: "ContractTest", code: 101,
            userInfo: [NSLocalizedDescriptionKey: "Missing source marker: \(startMarker)"])
    }
    guard let end = source.range(of: endMarker, range: start..<source.endIndex)?.lowerBound else {
        throw NSError(domain: "ContractTest", code: 102,
            userInfo: [NSLocalizedDescriptionKey: "Missing source marker: \(endMarker)"])
    }
    return String(source[start..<end])
}

func cliSourceContent(at sourceURL: URL = URL(fileURLWithPath: "Homeboy/Core/CLI")) throws -> String {
    try FileManager.default.contentsOfDirectory(
        at: sourceURL,
        includingPropertiesForKeys: nil
    )
    .filter { $0.pathExtension == "swift" }
    .sorted { $0.lastPathComponent < $1.lastPathComponent }
    .map { try String(contentsOf: $0, encoding: .utf8) }
    .joined(separator: "\n")
}

func assertContains(_ haystack: String, _ needle: String, message: String) throws {
    guard haystack.contains(needle) else {
        throw NSError(domain: "ContractTest", code: 103,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}

func assertDoesNotContain(_ haystack: String, _ needle: String, message: String) throws {
    guard !haystack.contains(needle) else {
        throw NSError(domain: "ContractTest", code: 104,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}
