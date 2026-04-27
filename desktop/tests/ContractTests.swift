#!/usr/bin/env swift

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

struct ComponentConfigurationTest: Decodable {
    let id: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
    let versionTargets: [VersionTargetTest]?
    let buildCommand: String?

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

// MARK: - Test Runner

func runTests(testDir: String) throws {
    let fixturesDir = "\(testDir)/fixtures"
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase

    print("Contract Tests")
    print("==============")
    print("Fixtures: \(fixturesDir)")
    print("")

    // Test 1: deploy-dry-run.json parsing
    try testDeployDryRun(fixturesDir: fixturesDir, decoder: decoder)

    // Test 2: component-list.json parsing
    try testComponentList(fixturesDir: fixturesDir, decoder: decoder)

    // Test 3: db-describe.json parsing
    try testDbDescribe(fixturesDir: fixturesDir, decoder: decoder)

    // Test 4: component.json parsing (ComponentConfiguration)
    try testComponentConfigurationFullDecode(fixturesDir: fixturesDir, decoder: decoder)

    // Test 5: minimal component (required fields only)
    try testComponentConfigurationMinimal(decoder: decoder)

    // Test 6: displayName computation
    try testDisplayNameComputation()

    // Test 7: versionTargets parsing
    try testVersionTargetsParsing(fixturesDir: fixturesDir, decoder: decoder)

    // Test 8: Remote Log Viewer pin command shape
    try testRemoteLogViewerPinCommandShape(testDir: testDir)

    // Test 9: HomeboyCLI helper command shapes
    try testHomeboyCLIHelperCommandShapes()

    // Test 10: extension-list.json parsing
    try testExtensionList(fixturesDir: fixturesDir, decoder: decoder)

    // Test 11: mutation command construction stays aligned with current homeboy parser
    try testMutationCommandShapes(testDir: testDir)

    // Test 12: CLI command construction uses current Homeboy surface
    try testCurrentCLICommandSurface(testDir: testDir)

    // Test 13: bench result parsing
    try testBenchResult(fixturesDir: fixturesDir, decoder: decoder)

    // Test 14: release/build planning model parsing
    try testReleaseBuildPlanningFixtures(fixturesDir: fixturesDir, decoder: decoder)

    // Test 15: release/build planning command shapes
    try testReleaseBuildPlanningCommandShapes()

    // Test 16: Rig command JSON contracts
    try testRigContracts(fixturesDir: fixturesDir, decoder: decoder)

    // Test 17: Stack command JSON contracts
    try testStackContracts(fixturesDir: fixturesDir, decoder: decoder)

    // Test 18: git-status.json parsing
    try testGitStatus(fixturesDir: fixturesDir, decoder: decoder)

    // Test 19: Git workspace command shapes
    try testGitWorkspaceCommandShapes(testDir: testDir)

    // Test 20: API/Auth model decoding and command shapes
    try testAPIAuthContracts(decoder: decoder)

    // Test 21: Quality workspace command shapes
    try testQualityWorkspaceCommandShapes(testDir: testDir)

    print("")
    print("All contract tests passed")
}

func testAPIAuthContracts(decoder: JSONDecoder) throws {
    print("Test: API/Auth workspace contracts")
    print("----------------------------------")

    let authJSON = #"{"success":true,"data":{"project_id":"example","authenticated":true}}"#.data(using: .utf8)!
    let auth = try decoder.decode(CLIResponse<HomeboyAuthOutputTest>.self, from: authJSON)
    guard auth.data?.projectId == "example", auth.data?.authenticated == true else {
        throw NSError(domain: "ContractTest", code: 120,
            userInfo: [NSLocalizedDescriptionKey: "auth status output does not decode snake_case project_id/authenticated fields"])
    }
    print("[PASS] Auth status output decodes")

    let apiJSON = #"{"success":true,"data":{"project_id":"example","method":"GET","endpoint":"/wp/v2/types","response":{"ok":true,"count":2}}}"#.data(using: .utf8)!
    let api = try decoder.decode(CLIResponse<HomeboyAPIGetOutputTest>.self, from: apiJSON)
    guard api.data?.projectId == "example", api.data?.method == "GET", api.data?.endpoint == "/wp/v2/types" else {
        throw NSError(domain: "ContractTest", code: 121,
            userInfo: [NSLocalizedDescriptionKey: "api GET output does not decode expected metadata fields"])
    }
    guard case .object(let responseObject) = api.data?.response,
          case .bool(true) = responseObject["ok"],
          case .int(2) = responseObject["count"] else {
        throw NSError(domain: "ContractTest", code: 122,
            userInfo: [NSLocalizedDescriptionKey: "api GET response does not preserve arbitrary JSON"])
    }
    print("[PASS] API GET output decodes arbitrary response JSON")

    let source = try String(contentsOf: URL(fileURLWithPath: "Homeboy/Core/CLI/HomeboyCLI.swift"), encoding: .utf8)

    func requireContains(_ needle: String, _ message: String, code: Int) throws {
        guard source.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    try requireContains("[\"auth\", \"status\", \"--project\", projectId]",
        "auth status wrapper uses current CLI shape", code: 123)
    try requireContains("[\"auth\", \"login\", \"--project\", projectId, \"--identifier\", identifier, \"--password\", password]",
        "auth login wrapper uses non-interactive CLI flags", code: 124)
    try requireContains("[\"auth\", \"logout\", \"--project\", projectId]",
        "auth logout wrapper uses current CLI shape", code: 125)
    try requireContains("[\"api\", projectId, \"get\", endpoint]",
        "api GET wrapper exposes only read-only API command", code: 126)

    print("")
}

func testReleaseBuildPlanningFixtures(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: release/build planning fixtures")
    print("-------------------------------------")

    let changesData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/changes.json"))
    let changes = try decoder.decode(CLIResponse<ChangesOutputTest>.self, from: changesData)
    guard changes.success, let changesOutput = changes.data else {
        throw NSError(domain: "ContractTest", code: 92,
            userInfo: [NSLocalizedDescriptionKey: "changes.json did not decode as a successful response"])
    }
    guard changesOutput.componentId == "homeboy-desktop" else {
        throw NSError(domain: "ContractTest", code: 93,
            userInfo: [NSLocalizedDescriptionKey: "changes.json component_id mismatch"])
    }
    guard changesOutput.commits.count == 2 else {
        throw NSError(domain: "ContractTest", code: 94,
            userInfo: [NSLocalizedDescriptionKey: "changes.json expected 2 commits"])
    }
    print("[PASS] changes output decodes with commits and baseline")

    let versionData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/version-show.json"))
    let version = try decoder.decode(CLIResponse<VersionShowOutputTest>.self, from: versionData)
    guard version.success, version.data?.version == "0.11.3" else {
        throw NSError(domain: "ContractTest", code: 95,
            userInfo: [NSLocalizedDescriptionKey: "version-show.json did not decode expected version"])
    }
    print("[PASS] version show output decodes")

    let releaseData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/release-dry-run.json"))
    let release = try decoder.decode(CLIResponse<ReleaseOutputTest>.self, from: releaseData)
    guard release.success, release.data?.result?.dryRun == true else {
        throw NSError(domain: "ContractTest", code: 96,
            userInfo: [NSLocalizedDescriptionKey: "release-dry-run.json did not decode dry_run=true"])
    }
    guard release.data?.result?.skippedReason == "major-requires-flag" else {
        throw NSError(domain: "ContractTest", code: 97,
            userInfo: [NSLocalizedDescriptionKey: "release-dry-run.json skipped_reason mismatch"])
    }
    print("[PASS] release dry-run output decodes")

    print("")
}

func testReleaseBuildPlanningCommandShapes() throws {
    print("Test: release/build planning command shapes")
    print("-------------------------------------------")

    let source = try String(contentsOf: URL(fileURLWithPath: "Homeboy/Core/CLI/HomeboyCLI.swift"), encoding: .utf8)
    let contentView = try String(contentsOf: URL(fileURLWithPath: "Homeboy/App/ContentView.swift"), encoding: .utf8)

    func requireContains(_ haystack: String, _ needle: String, _ message: String, code: Int) throws {
        guard haystack.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    func requireNotContains(_ haystack: String, _ needle: String, _ message: String, code: Int) throws {
        guard !haystack.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    try requireContains(source, "[\"changes\", componentId]", "changes helper uses current command shape", code: 98)
    try requireContains(source, "[\"version\", \"show\", componentId]", "version show helper uses current command shape", code: 99)
    try requireContains(source, "[\"build\", componentId]", "build helper uses current command shape", code: 100)
    try requireContains(source, "[\"release\", componentId, \"--dry-run\"]", "release helper is dry-run only", code: 101)
    try requireContains(contentView, "case release = \"Release\"", "release is a separate core tool", code: 102)
    try requireContains(contentView, "ReleaseWorkflowView()", "release view is mounted separately from deployer", code: 103)
    try requireContains(contentView, "Release Disabled", "mutating release action remains disabled", code: 104)
    try requireNotContains(contentView, "cli.release(", "release view does not call a mutating release helper", code: 105)

    print("")
}

func testGitStatus(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: git-status.json")
    print("---------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/git-status.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 100,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: git-status.json"])
    }

    let data = try Data(contentsOf: fixture)
    let result = try decoder.decode(CLIResponse<GitStatusOutputTest>.self, from: data)

    guard result.success else {
        throw NSError(domain: "ContractTest", code: 101,
            userInfo: [NSLocalizedDescriptionKey: "git-status.json: success=false"])
    }

    guard let status = result.data else {
        throw NSError(domain: "ContractTest", code: 102,
            userInfo: [NSLocalizedDescriptionKey: "git-status.json: data field is nil"])
    }

    guard status.action == "status" else {
        throw NSError(domain: "ContractTest", code: 103,
            userInfo: [NSLocalizedDescriptionKey: "git-status.json: action mismatch"])
    }
    print("[PASS] action is status")

    guard status.componentId == "homeboy-desktop" else {
        throw NSError(domain: "ContractTest", code: 104,
            userInfo: [NSLocalizedDescriptionKey: "git-status.json: componentId mismatch"])
    }
    print("[PASS] componentId decoded")

    guard status.exitCode == 0 && status.success else {
        throw NSError(domain: "ContractTest", code: 105,
            userInfo: [NSLocalizedDescriptionKey: "git-status.json: status should be successful"])
    }
    print("[PASS] status success fields decoded")
    print("")
}

func testQualityWorkspaceCommandShapes(testDir: String) throws {
    print("Test: Quality workspace command shapes")
    print("--------------------------------------")

    let sourceURL = URL(fileURLWithPath: "Homeboy/Core/CLI/HomeboyCLI.swift")
    let contentViewURL = URL(fileURLWithPath: "Homeboy/App/ContentView.swift")
    let qualityViewURL = URL(fileURLWithPath: "Homeboy/Extensions/Quality/Views/QualityView.swift")
    let qualityViewModelURL = URL(fileURLWithPath: "Homeboy/Extensions/Quality/QualityViewModel.swift")

    let source = try String(contentsOf: sourceURL, encoding: .utf8)
    let contentView = try String(contentsOf: contentViewURL, encoding: .utf8)
    let qualityView = try String(contentsOf: qualityViewURL, encoding: .utf8)
    let qualityViewModel = try String(contentsOf: qualityViewModelURL, encoding: .utf8)

    func requireContains(_ haystack: String, _ needle: String, _ message: String, code: Int) throws {
        guard haystack.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    func requireNotContains(_ haystack: String, _ needle: String, _ message: String, code: Int) throws {
        guard !haystack.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    try requireContains(contentView, "case quality = \"Quality\"", "Quality is registered as a core tool", code: 100)
    try requireContains(contentView, "QualityView()", "QualityView is mounted in the core tool stack", code: 101)
    try requireContains(source, #"["review", componentId, "--summary"]"#, "Quality review uses current review --summary command", code: 102)
    try requireContains(source, #"["triage", "component", componentId]"#, "Quality triage uses current triage component command", code: 103)
    try requireContains(source, #"args.append(contentsOf: ["--changed-since", changedSince])"#, "Quality scope supports changed-since", code: 104)
    try requireContains(source, #"args.append("--changed-only")"#, "Quality scope supports changed-only", code: 105)
    try requireContains(source, #"args.append(contentsOf: ["--path", path])"#, "Quality commands support component path override", code: 106)
    try requireContains(source, "executeCommandWithOutputFile", "Quality decoding reads --output JSON envelopes", code: 107)
    try requireContains(qualityView, "Run Review", "Quality UI exposes review entry point", code: 108)
    try requireContains(qualityView, "Run Triage", "Quality UI exposes triage entry point", code: 109)
    try requireContains(qualityView, "Findings:", "Quality UI renders stage finding summaries", code: 110)
    try requireContains(qualityViewModel, "HomeboyCLI.shared.qualityStage", "Quality view model exposes stage deep-dives", code: 111)

    try requireNotContains(source, #"["audit", "code""#, "Removed stale audit code invocation", code: 112)
    try requireNotContains(source, #"["audit", "docs""#, "Removed stale audit docs invocation", code: 113)
    try requireNotContains(source, #"["audit", "structure""#, "Removed stale audit structure invocation", code: 114)
    try requireNotContains(source, #"["supports""#, "Removed stale supports command invocation", code: 115)
    try requireNotContains(qualityView, "--fix", "Quality UI does not expose lint --fix", code: 116)
    try requireNotContains(qualityView, "--write", "Quality UI does not expose write actions", code: 117)
    try requireNotContains(source, "--baseline", "Quality helpers do not expose baseline writes", code: 118)
    try requireNotContains(source, "--ratchet", "Quality helpers do not expose ratchet writes", code: 119)

    print("")
}

func testDeployDryRun(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: deploy-dry-run.json")
    print("-------------------------")

    let deployFixture = URL(fileURLWithPath: "\(fixturesDir)/deploy-dry-run.json")

    guard FileManager.default.fileExists(atPath: deployFixture.path) else {
        throw NSError(domain: "ContractTest", code: 1,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: deploy-dry-run.json"])
    }

    let deployData = try Data(contentsOf: deployFixture)

    // Attempt to decode through CLIResponse wrapper (matches DeployerViewModel)
    let deployResult = try decoder.decode(CLIResponse<CLIDeploymentResult>.self, from: deployData)

    guard deployResult.success else {
        throw NSError(domain: "ContractTest", code: 2,
            userInfo: [NSLocalizedDescriptionKey: "deploy-dry-run.json: success=false"])
    }

    guard let data = deployResult.data else {
        throw NSError(domain: "ContractTest", code: 3,
            userInfo: [NSLocalizedDescriptionKey: "deploy-dry-run.json: data field is nil"])
    }

    print("[PASS] Parsed CLIResponse wrapper")
    print("[PASS] Parsed \(data.results.count) components from deploy-dry-run.json")

    // Validate component fields
    for result in data.results {
        guard !result.id.isEmpty else {
            throw NSError(domain: "ContractTest", code: 4,
                userInfo: [NSLocalizedDescriptionKey: "Component ID is empty"])
        }
        guard !result.status.isEmpty else {
            throw NSError(domain: "ContractTest", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "Component status is empty for \(result.id)"])
        }
    }
    print("[PASS] All components have id and status")

    // Validate summary total matches results count
    guard data.summary.total == data.results.count else {
        throw NSError(domain: "ContractTest", code: 6,
            userInfo: [NSLocalizedDescriptionKey: "Summary total (\(data.summary.total)) doesn't match results count (\(data.results.count))"])
    }
    print("[PASS] Summary total matches results count")

    // Print summary
    print("")
    print("Deploy Summary:")
    print("  Components: \(data.results.count)")
    print("  Succeeded: \(data.summary.succeeded)")
    print("  Failed: \(data.summary.failed)")
    print("  Skipped: \(data.summary.skipped)")
    print("")
}

func testComponentList(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: component-list.json")
    print("-------------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/component-list.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 10,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: component-list.json"])
    }

    let data = try Data(contentsOf: fixture)

    // Decode through CLIResponse wrapper (matches HomeboyCLI)
    let result = try decoder.decode(CLIResponse<ComponentListOutput>.self, from: data)

    guard result.success else {
        throw NSError(domain: "ContractTest", code: 11,
            userInfo: [NSLocalizedDescriptionKey: "component-list.json: success=false"])
    }

    guard let output = result.data else {
        throw NSError(domain: "ContractTest", code: 12,
            userInfo: [NSLocalizedDescriptionKey: "component-list.json: data field is nil"])
    }

    print("[PASS] Parsed CLIResponse wrapper")

    guard let components = output.entities else {
        throw NSError(domain: "ContractTest", code: 13,
            userInfo: [NSLocalizedDescriptionKey: "component-list.json: entities field is nil"])
    }

    print("[PASS] Parsed \(components.count) components from component-list.json")

    // Validate component fields
    for component in components {
        guard !component.id.isEmpty else {
            throw NSError(domain: "ContractTest", code: 14,
                userInfo: [NSLocalizedDescriptionKey: "Component ID is empty"])
        }
        guard !component.localPath.isEmpty else {
            throw NSError(domain: "ContractTest", code: 15,
                userInfo: [NSLocalizedDescriptionKey: "Component localPath is empty for \(component.id)"])
        }
    }
    print("[PASS] All components have id and localPath")

    // Count components with build artifacts
    let withArtifacts = components.filter { $0.buildArtifact != nil }.count
    print("")
    print("Component Summary:")
    print("  Total: \(components.count)")
    print("  With build artifacts: \(withArtifacts)")
    print("")
}

func testDbDescribe(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: db-describe.json")
    print("----------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/db-describe.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 20,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: db-describe.json"])
    }

    let data = try Data(contentsOf: fixture)

    // Decode through CLIResponse wrapper (matches HomeboyCLI)
    let result = try decoder.decode(CLIResponse<DbOutput>.self, from: data)

    guard result.success else {
        throw NSError(domain: "ContractTest", code: 21,
            userInfo: [NSLocalizedDescriptionKey: "db-describe.json: success=false"])
    }

    guard let output = result.data else {
        throw NSError(domain: "ContractTest", code: 22,
            userInfo: [NSLocalizedDescriptionKey: "db-describe.json: data field is nil"])
    }

    print("[PASS] Parsed CLIResponse wrapper")
    print("[PASS] Parsed DbOutput with projectId: \(output.projectId)")

    // Validate stdout contains parseable table data
    guard let stdout = output.stdout else {
        throw NSError(domain: "ContractTest", code: 23,
            userInfo: [NSLocalizedDescriptionKey: "db-describe.json: stdout is nil"])
    }

    guard let tableData = stdout.data(using: .utf8) else {
        throw NSError(domain: "ContractTest", code: 24,
            userInfo: [NSLocalizedDescriptionKey: "db-describe.json: stdout is not valid UTF-8"])
    }

    // Parse tables from stdout (matches DatabaseBrowserViewModel.parseTables)
    let tables = try JSONDecoder().decode([WPTable].self, from: tableData)

    print("[PASS] Parsed \(tables.count) tables from stdout")

    // Validate table fields
    for table in tables {
        guard !table.Name.isEmpty else {
            throw NSError(domain: "ContractTest", code: 25,
                userInfo: [NSLocalizedDescriptionKey: "Table Name is empty"])
        }
    }
    print("[PASS] All tables have Name field")

    print("")
    print("Database Summary:")
    print("  Tables: \(tables.count)")
    for table in tables {
        print("    - \(table.Name) (\(table.Rows ?? "?") rows)")
    }
    print("")
}

func testComponentConfigurationFullDecode(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: component.json (full decode)")
    print("----------------------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/component.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 30,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: component.json"])
    }

    let data = try Data(contentsOf: fixture)
    let component = try decoder.decode(ComponentConfigurationTest.self, from: data)

    print("[PASS] Decoded ComponentConfiguration")

    // Validate required fields
    guard !component.id.isEmpty else {
        throw NSError(domain: "ContractTest", code: 31,
            userInfo: [NSLocalizedDescriptionKey: "component.json: id is empty"])
    }
    print("[PASS] id: \(component.id)")

    guard !component.localPath.isEmpty else {
        throw NSError(domain: "ContractTest", code: 32,
            userInfo: [NSLocalizedDescriptionKey: "component.json: localPath is empty"])
    }
    print("[PASS] localPath: \(component.localPath)")

    guard !component.remotePath.isEmpty else {
        throw NSError(domain: "ContractTest", code: 33,
            userInfo: [NSLocalizedDescriptionKey: "component.json: remotePath is empty"])
    }
    print("[PASS] remotePath: \(component.remotePath)")

    // Validate optional fields
    guard component.buildArtifact != nil else {
        throw NSError(domain: "ContractTest", code: 34,
            userInfo: [NSLocalizedDescriptionKey: "component.json: buildArtifact should be present"])
    }
    print("[PASS] buildArtifact: \(component.buildArtifact!)")

    guard component.buildCommand != nil else {
        throw NSError(domain: "ContractTest", code: 35,
            userInfo: [NSLocalizedDescriptionKey: "component.json: buildCommand should be present"])
    }
    print("[PASS] buildCommand: \(component.buildCommand!)")

    guard let versionTargets = component.versionTargets, !versionTargets.isEmpty else {
        throw NSError(domain: "ContractTest", code: 36,
            userInfo: [NSLocalizedDescriptionKey: "component.json: versionTargets should be present and non-empty"])
    }
    print("[PASS] versionTargets: \(versionTargets.count) targets")
    print("")
}

func testComponentConfigurationMinimal(decoder: JSONDecoder) throws {
    print("Test: component minimal (required fields only)")
    print("----------------------------------------------")

    // Minimal JSON with only required fields
    let minimalJson = """
    {
        "id": "my-plugin",
        "local_path": "/path/to/plugin",
        "remote_path": "wp-content/plugins/my-plugin"
    }
    """

    let data = minimalJson.data(using: .utf8)!
    let component = try decoder.decode(ComponentConfigurationTest.self, from: data)

    print("[PASS] Decoded minimal ComponentConfiguration")

    guard component.id == "my-plugin" else {
        throw NSError(domain: "ContractTest", code: 40,
            userInfo: [NSLocalizedDescriptionKey: "Minimal: id mismatch"])
    }
    print("[PASS] id matches")

    guard component.buildArtifact == nil else {
        throw NSError(domain: "ContractTest", code: 41,
            userInfo: [NSLocalizedDescriptionKey: "Minimal: buildArtifact should be nil"])
    }
    print("[PASS] buildArtifact is nil")

    guard component.versionTargets == nil else {
        throw NSError(domain: "ContractTest", code: 42,
            userInfo: [NSLocalizedDescriptionKey: "Minimal: versionTargets should be nil"])
    }
    print("[PASS] versionTargets is nil")

    guard component.buildCommand == nil else {
        throw NSError(domain: "ContractTest", code: 43,
            userInfo: [NSLocalizedDescriptionKey: "Minimal: buildCommand should be nil"])
    }
    print("[PASS] buildCommand is nil")
    print("")
}

func testDisplayNameComputation() throws {
    print("Test: displayName computation")
    print("-----------------------------")

    // Test cases for displayName computation
    let testCases: [(id: String, expected: String)] = [
        ("my-plugin", "My Plugin"),
        ("extra-chill-theme", "Extra Chill Theme"),
        ("simple", "Simple"),
        ("a-b-c-d", "A B C D"),
    ]

    for testCase in testCases {
        let computed = testCase.id.split(separator: "-")
            .map { $0.capitalized }
            .joined(separator: " ")

        guard computed == testCase.expected else {
            throw NSError(domain: "ContractTest", code: 50,
                userInfo: [NSLocalizedDescriptionKey: "displayName: '\(testCase.id)' -> '\(computed)' (expected '\(testCase.expected)')"])
        }
        print("[PASS] '\(testCase.id)' -> '\(computed)'")
    }
    print("")
}

func testVersionTargetsParsing(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: versionTargets parsing")
    print("----------------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/component.json")
    let data = try Data(contentsOf: fixture)
    let component = try decoder.decode(ComponentConfigurationTest.self, from: data)

    guard let targets = component.versionTargets else {
        throw NSError(domain: "ContractTest", code: 60,
            userInfo: [NSLocalizedDescriptionKey: "versionTargets is nil"])
    }
    print("[PASS] versionTargets array decoded")

    guard targets.count == 2 else {
        throw NSError(domain: "ContractTest", code: 61,
            userInfo: [NSLocalizedDescriptionKey: "Expected 2 version targets, got \(targets.count)"])
    }
    print("[PASS] versionTargets has 2 entries")

    // First target should have both file and pattern
    let first = targets[0]
    guard first.file == "style.css" else {
        throw NSError(domain: "ContractTest", code: 62,
            userInfo: [NSLocalizedDescriptionKey: "First target file mismatch: \(first.file)"])
    }
    print("[PASS] First target file: \(first.file)")

    guard first.pattern != nil else {
        throw NSError(domain: "ContractTest", code: 63,
            userInfo: [NSLocalizedDescriptionKey: "First target pattern should not be nil"])
    }
    print("[PASS] First target has pattern")

    // Second target should have file only (pattern is optional)
    let second = targets[1]
    guard second.file == "functions.php" else {
        throw NSError(domain: "ContractTest", code: 64,
            userInfo: [NSLocalizedDescriptionKey: "Second target file mismatch: \(second.file)"])
    }
    print("[PASS] Second target file: \(second.file)")

    guard second.pattern == nil else {
        throw NSError(domain: "ContractTest", code: 65,
            userInfo: [NSLocalizedDescriptionKey: "Second target pattern should be nil"])
    }
    print("[PASS] Second target pattern is nil (optional field)")

    // Test computed properties
    guard component.versionFile == "style.css" else {
        throw NSError(domain: "ContractTest", code: 66,
            userInfo: [NSLocalizedDescriptionKey: "versionFile computed property mismatch"])
    }
    print("[PASS] versionFile computed property: \(component.versionFile!)")

    guard component.versionPattern != nil else {
        throw NSError(domain: "ContractTest", code: 67,
            userInfo: [NSLocalizedDescriptionKey: "versionPattern computed property should not be nil"])
    }
    print("[PASS] versionPattern computed property: present")
    print("")
}

func testRemoteLogViewerPinCommandShape(testDir: String) throws {
    print("Test: Remote Log Viewer pin command shape")
    print("-----------------------------------------")

    let sourcePath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Extensions/RemoteLogViewer/RemoteLogViewerViewModel.swift")

    let source = try String(contentsOf: sourcePath, encoding: .utf8)
    let currentPinCommand = #"["project", "pin", "add", "--type", "log", "--tail", String(log.tailLines), projectId, log.path]"#
    let currentTailUpdateCommand = #"["project", "pin", "add", "--type", "log", "--tail", String(lines), projectId, log.path]"#
    let oldPositionalFirstCommand = #"["project", "pin", "add", projectId, log.path, "--type", "log", "--tail"#

    guard source.contains(currentPinCommand) else {
        throw NSError(domain: "ContractTest", code: 70,
            userInfo: [NSLocalizedDescriptionKey: "RemoteLogViewer pin command does not use current homeboy project pin add syntax"])
    }
    print("[PASS] Pin command puts --type/--tail before project/path")

    guard source.contains(currentTailUpdateCommand) else {
        throw NSError(domain: "ContractTest", code: 71,
            userInfo: [NSLocalizedDescriptionKey: "RemoteLogViewer tail update command does not preserve current pin syntax"])
    }
    print("[PASS] Tail-line update command preserves --tail before project/path")

    guard !source.contains(oldPositionalFirstCommand) else {
        throw NSError(domain: "ContractTest", code: 72,
            userInfo: [NSLocalizedDescriptionKey: "RemoteLogViewer still contains old positional-first project pin add syntax"])
    }
    print("[PASS] Old positional-first log pin syntax is absent")

    print("")
}

func testHomeboyCLIHelperCommandShapes() throws {
    print("Test: HomeboyCLI helper command shapes")
    print("--------------------------------------")

    let sourceURL = URL(fileURLWithPath: "Homeboy/Core/CLI/HomeboyCLI.swift")
    guard FileManager.default.fileExists(atPath: sourceURL.path) else {
        throw NSError(domain: "ContractTest", code: 80,
            userInfo: [NSLocalizedDescriptionKey: "Source file not found: \(sourceURL.path)"])
    }

    let source = try String(contentsOf: sourceURL, encoding: .utf8)

    func requireContains(_ needle: String, _ message: String, code: Int) throws {
        guard source.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    func requireNotContains(_ needle: String, _ message: String, code: Int) throws {
        guard !source.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    try requireContains("[\"refactor\", \"rename\", \"--from\", from, \"--to\", to, \"--component\", componentId]",
        "refactor rename uses current component flag shape", code: 81)
    try requireContains("[\"--file-type\", type]",
        "file find uses --file-type", code: 82)
    try requireContains("[\"--limit\", String(limit)]",
        "db search limit is converted before parser validation", code: 83)
    try requireContains("[\"--max-depth\", String(depth)]",
        "file search depth is converted before parser validation", code: 84)
    try requireContains("[\"-n\", String(lines)]",
        "log line counts are converted before parser validation", code: 85)
    try requireContains("[\"-C\", String(context)]",
        "log context counts are converted before parser validation", code: 86)

    try requireNotContains("[\"--type\", type]",
        "file find no longer uses stale --type flag", code: 87)
    try requireNotContains(#"\(lines)"#,
        "helper commands do not pass literal line interpolation templates", code: 88)
    try requireNotContains(#"\(depth)"#,
        "helper commands do not pass literal depth interpolation templates", code: 89)
    try requireNotContains(#"\(limit)"#,
        "helper commands do not pass literal limit interpolation templates", code: 90)
    try requireNotContains(#"\(context)"#,
        "helper commands do not pass literal context interpolation templates", code: 91)

    print("")
}

func testExtensionList(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: extension-list.json")
    print("-------------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/extension-list.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 70,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: extension-list.json"])
    }

    let data = try Data(contentsOf: fixture)
    let result = try decoder.decode(CLIResponse<ExtensionListOutput>.self, from: data)

    guard result.success else {
        throw NSError(domain: "ContractTest", code: 71,
            userInfo: [NSLocalizedDescriptionKey: "extension-list.json: success=false"])
    }
    print("[PASS] Parsed CLIResponse wrapper")

    guard let output = result.data else {
        throw NSError(domain: "ContractTest", code: 72,
            userInfo: [NSLocalizedDescriptionKey: "extension-list.json: data field is nil"])
    }

    guard output.command == "extension.list" else {
        throw NSError(domain: "ContractTest", code: 73,
            userInfo: [NSLocalizedDescriptionKey: "extension-list.json: command mismatch"])
    }
    print("[PASS] Parsed extension.list command")

    guard output.projectId == nil else {
        throw NSError(domain: "ContractTest", code: 74,
            userInfo: [NSLocalizedDescriptionKey: "extension-list.json should not be project-scoped"])
    }
    print("[PASS] Extension list is not project-scoped")

    guard output.extensions.count == 2 else {
        throw NSError(domain: "ContractTest", code: 75,
            userInfo: [NSLocalizedDescriptionKey: "Expected 2 extensions, got \(output.extensions.count)"])
    }
    print("[PASS] Parsed \(output.extensions.count) extensions")

    let node = output.extensions[0]
    guard node.configured == nil else {
        throw NSError(domain: "ContractTest", code: 76,
            userInfo: [NSLocalizedDescriptionKey: "Current extension list output should not require configured"])
    }
    print("[PASS] Optional configured field may be absent")

    guard node.actions?.first?.id == "release.package" else {
        throw NSError(domain: "ContractTest", code: 77,
            userInfo: [NSLocalizedDescriptionKey: "Platform extension action did not decode"])
    }
    print("[PASS] Platform extension actions decoded")

    let swift = output.extensions[1]
    guard swift.hasSetup == true && swift.hasReadyCheck == true else {
        throw NSError(domain: "ContractTest", code: 78,
            userInfo: [NSLocalizedDescriptionKey: "Executable extension readiness flags did not decode"])
    }
    print("[PASS] Executable extension readiness flags decoded")
    print("")
}

func testMutationCommandShapes(testDir: String) throws {
    print("Test: mutation command shapes")
    print("-----------------------------")

    let testURL = URL(fileURLWithPath: testDir)
    let repoRoot = testURL.lastPathComponent == "tests" ? testURL.deletingLastPathComponent() : testURL
    let cliURL = repoRoot.appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI.swift")

    guard FileManager.default.fileExists(atPath: cliURL.path) else {
        throw NSError(domain: "ContractTest", code: 100,
            userInfo: [NSLocalizedDescriptionKey: "HomeboyCLI.swift not found at \(cliURL.path)"])
    }

    let source = try String(contentsOf: cliURL, encoding: .utf8)
    let fleetCreate = try sourceSlice(
        source,
        from: "func fleetCreate(",
        to: "    /// Delete a fleet"
    )
    let componentCreate = try sourceSlice(
        source,
        from: "func componentCreate(",
        to: "    func componentSet("
    )

    try assertContains(fleetCreate, "\"--projects\"", message: "fleet create must use current --projects flag")
    try assertDoesNotContain(fleetCreate, "\"--project\"", message: "fleet create must not use legacy repeated --project flag")
    try assertContains(fleetCreate, "joined(separator: \",\")", message: "fleet create should pass comma-separated project IDs")
    print("[PASS] fleet create uses --projects")

    try assertContains(componentCreate, "\"--local-path\"", message: "component create must pass local path as an explicit flag")
    try assertContains(componentCreate, "\"--remote-path\"", message: "component create must pass remote path as an explicit flag")
    try assertContains(componentCreate, "\"--build-artifact\"", message: "component create keeps build artifact flag")
    try assertDoesNotContain(componentCreate, "\"component\", \"create\", name, localPath, remotePath", message: "component create must not use legacy positional name/localPath/remotePath")
    print("[PASS] component create uses explicit flags")
    print("")
}

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

func testCurrentCLICommandSurface(testDir: String) throws {
    print("Test: current CLI command surface")
    print("---------------------------------")

    let source = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI.swift")
    let content = try String(contentsOf: source, encoding: .utf8)

    let removedShapes = [
        "\"audit\", \"code\"",
        "\"audit\", \"docs\"",
        "\"audit\", \"structure\"",
        "\"supports\"",
        "SupportsOutput",
    ]

    for shape in removedShapes {
        guard !content.contains(shape) else {
            throw NSError(domain: "ContractTest", code: 70,
                userInfo: [NSLocalizedDescriptionKey: "Removed CLI shape still appears in HomeboyCLI.swift: \(shape)"])
        }
    }
    print("[PASS] Removed audit/supports command shapes are absent")

    let requiredShapes = [
        "var args = [\"audit\", componentId]",
        "args.append(contentsOf: [\"--only\", kind])",
        "args.append(\"--help\")",
        "commandSurfaceSupports",
    ]

    for shape in requiredShapes {
        guard content.contains(shape) else {
            throw NSError(domain: "ContractTest", code: 71,
                userInfo: [NSLocalizedDescriptionKey: "Expected CLI construction shape is missing: \(shape)"])
        }
    }
    print("[PASS] Audit filters and help-surface probe are present")

    print("")
}

func testGitWorkspaceCommandShapes(testDir: String) throws {
    print("Test: Git workspace command shapes")
    print("----------------------------------")

    let cliSource = try String(contentsOf: URL(fileURLWithPath: "Homeboy/Core/CLI/HomeboyCLI.swift"), encoding: .utf8)
    let viewModelSource = try String(
        contentsOf: URL(fileURLWithPath: "Homeboy/Extensions/GitOperations/GitOperationsViewModel.swift"),
        encoding: .utf8
    )
    let viewSource = try String(
        contentsOf: URL(fileURLWithPath: "Homeboy/Extensions/GitOperations/Views/GitOperationsView.swift"),
        encoding: .utf8
    )

    func requireContains(_ source: String, _ needle: String, _ message: String, code: Int) throws {
        guard source.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    func requireNotContains(_ source: String, _ needle: String, _ message: String, code: Int) throws {
        guard !source.contains(needle) else {
            throw NSError(domain: "ContractTest", code: code,
                userInfo: [NSLocalizedDescriptionKey: message])
        }
        print("[PASS] \(message)")
    }

    try requireContains(cliSource, #"var args = ["git", "status", componentId]"#,
        "Git status wrapper uses homeboy git status", code: 110)
    try requireContains(cliSource, #"args.append(contentsOf: ["--path", path])"#,
        "Git status wrapper supports --path", code: 111)
    try requireContains(viewModelSource, #"process.arguments = ["git", "-C", path, "remote", "get-url", "origin"]"#,
        "GitHub navigation reads origin URL without mutation", code: 112)
    try requireContains(viewSource, #"Button("Issues")"#,
        "Git view exposes Issues navigation", code: 113)
    try requireContains(viewSource, #"Button("Pull Requests")"#,
        "Git view exposes Pull Requests navigation", code: 114)

    for forbidden in ["commit", "push", "rebase", "cherry-pick", "tag", "issues reconcile"] {
        try requireNotContains(viewSource, forbidden, "Git workspace does not expose mutating \(forbidden) operation", code: 120)
    }

    print("")
}

func testBenchResult(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: bench-result.json")
    print("-----------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/bench-result.json")

    guard FileManager.default.fileExists(atPath: fixture.path) else {
        throw NSError(domain: "ContractTest", code: 80,
            userInfo: [NSLocalizedDescriptionKey: "Fixture not found: bench-result.json"])
    }

    let data = try Data(contentsOf: fixture)
    let result = try decoder.decode(CLIResponse<BenchCommandOutputTest>.self, from: data)

    guard result.success else {
        throw NSError(domain: "ContractTest", code: 81,
            userInfo: [NSLocalizedDescriptionKey: "bench-result.json: success=false"])
    }

    guard let output = result.data else {
        throw NSError(domain: "ContractTest", code: 82,
            userInfo: [NSLocalizedDescriptionKey: "bench-result.json: data field is nil"])
    }

    guard output.component == "homeboy" else {
        throw NSError(domain: "ContractTest", code: 83,
            userInfo: [NSLocalizedDescriptionKey: "Unexpected bench component: \(output.component)"])
    }
    print("[PASS] Parsed bench command output")

    guard let results = output.results else {
        throw NSError(domain: "ContractTest", code: 84,
            userInfo: [NSLocalizedDescriptionKey: "bench-result.json: results is nil"])
    }

    guard results.scenarios.count == 2 else {
        throw NSError(domain: "ContractTest", code: 85,
            userInfo: [NSLocalizedDescriptionKey: "Expected 2 scenarios, got \(results.scenarios.count)"])
    }
    print("[PASS] Parsed \(results.scenarios.count) bench scenarios")

    guard let p95 = results.scenarios.first?.metrics["p95_ms"], p95 > 0 else {
        throw NSError(domain: "ContractTest", code: 86,
            userInfo: [NSLocalizedDescriptionKey: "First scenario missing positive p95_ms"])
    }
    print("[PASS] Scenario metrics decode as numeric values")

    guard results.scenarios.first?.memory?.peakBytes == 41943040 else {
        throw NSError(domain: "ContractTest", code: 87,
            userInfo: [NSLocalizedDescriptionKey: "First scenario memory peak did not decode"])
    }
    print("[PASS] Scenario memory decodes from peak_bytes")

    guard output.baselineComparison?.regression == false else {
        throw NSError(domain: "ContractTest", code: 88,
            userInfo: [NSLocalizedDescriptionKey: "Baseline comparison regression flag mismatch"])
    }
    print("[PASS] Baseline comparison decodes")
    print("")
}

func testRigContracts(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: rig command contracts")
    print("---------------------------")

    let listFixture = URL(fileURLWithPath: "\(fixturesDir)/rig-list.json")
    let showFixture = URL(fileURLWithPath: "\(fixturesDir)/rig-show.json")
    let checkFixture = URL(fileURLWithPath: "\(fixturesDir)/rig-check-failed.json")

    let listData = try Data(contentsOf: listFixture)
    let listResult = try decoder.decode(CLIResponse<RigListOutputTest>.self, from: listData)
    guard listResult.success, let list = listResult.data, let rigs = list.rigs, !rigs.isEmpty else {
        throw NSError(domain: "ContractTest", code: 80,
            userInfo: [NSLocalizedDescriptionKey: "rig-list.json did not decode rigs"])
    }
    print("[PASS] Decoded rig list with \(rigs.count) rig(s)")

    let firstRig = rigs[0]
    guard firstRig.componentCount == 2, firstRig.serviceCount == 1 else {
        throw NSError(domain: "ContractTest", code: 81,
            userInfo: [NSLocalizedDescriptionKey: "rig-list.json counts did not decode"])
    }
    print("[PASS] Rig list counts decode")

    let showData = try Data(contentsOf: showFixture)
    let showResult = try decoder.decode(CLIResponse<RigShowOutputTest>.self, from: showData)
    guard showResult.success, let spec = showResult.data?.rig else {
        throw NSError(domain: "ContractTest", code: 82,
            userInfo: [NSLocalizedDescriptionKey: "rig-show.json did not decode rig spec"])
    }
    guard spec.components.keys.sorted() == ["studio", "wordpress-playground"] else {
        throw NSError(domain: "ContractTest", code: 83,
            userInfo: [NSLocalizedDescriptionKey: "rig-show.json component map did not decode"])
    }
    print("[PASS] Rig spec component map decodes")

    guard spec.pipeline?["check"]?.count == 2 else {
        throw NSError(domain: "ContractTest", code: 84,
            userInfo: [NSLocalizedDescriptionKey: "rig-show.json check pipeline did not decode"])
    }
    print("[PASS] Rig spec pipeline steps decode")

    let checkData = try Data(contentsOf: checkFixture)
    let checkResult = try decoder.decode(CLIResponse<RigCheckOutputTest>.self, from: checkData)
    guard checkResult.success == false, let check = checkResult.data else {
        throw NSError(domain: "ContractTest", code: 85,
            userInfo: [NSLocalizedDescriptionKey: "rig-check-failed.json should decode failed envelope with data"])
    }
    guard check.success == false, check.pipeline.failed == 1 else {
        throw NSError(domain: "ContractTest", code: 86,
            userInfo: [NSLocalizedDescriptionKey: "rig-check-failed.json failed check details did not decode"])
    }
    print("[PASS] Failed rig check data decodes without requiring success=true")
    print("")
}

func testStackContracts(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: stack command contracts")
    print("-----------------------------")

    let listData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/stack-list.json"))
    let listResult = try decoder.decode(CLIResponse<StackListOutput>.self, from: listData)
    guard listResult.success, let stacks = listResult.data?.stacks, stacks.count == 1 else {
        throw NSError(domain: "ContractTest", code: 90,
            userInfo: [NSLocalizedDescriptionKey: "stack-list.json did not decode one stack"])
    }
    guard stacks[0].id == "studio-combined", stacks[0].prCount == 5 else {
        throw NSError(domain: "ContractTest", code: 91,
            userInfo: [NSLocalizedDescriptionKey: "stack-list.json summary fields did not decode"])
    }
    print("[PASS] Stack list summary decodes")

    let statusData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/stack-status.json"))
    let statusResult = try decoder.decode(CLIResponse<StackStatusOutput>.self, from: statusData)
    guard statusResult.success, let status = statusResult.data else {
        throw NSError(domain: "ContractTest", code: 92,
            userInfo: [NSLocalizedDescriptionKey: "stack-status.json did not decode status data"])
    }
    guard status.stackId == "studio-combined", status.targetAhead == 7, status.targetBehind == 8 else {
        throw NSError(domain: "ContractTest", code: 93,
            userInfo: [NSLocalizedDescriptionKey: "stack-status.json target state did not decode"])
    }
    guard status.prs.first?.localState == "missing", status.prs.first?.reviewDecision == "REVIEW_REQUIRED" else {
        throw NSError(domain: "ContractTest", code: 94,
            userInfo: [NSLocalizedDescriptionKey: "stack-status.json PR state did not decode"])
    }
    print("[PASS] Stack status PR state decodes")
    print("")
}

// MARK: - Entry Point

let args = CommandLine.arguments
guard args.count > 1 else {
    print("Usage: ContractTests.swift <test-dir>")
    exit(1)
}

do {
    try runTests(testDir: args[1])
} catch {
    print("[FAIL] Contract test failed: \(error.localizedDescription)")
    exit(1)
}
