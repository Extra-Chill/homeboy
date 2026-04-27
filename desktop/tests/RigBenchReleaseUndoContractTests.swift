import Foundation

// MARK: - rig/bench/release/undo

func runRigBenchReleaseUndoContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testBenchResult(fixturesDir: fixturesDir, decoder: decoder)
    try testReleaseBuildPlanningFixtures(fixturesDir: fixturesDir, decoder: decoder)
    try testReleaseBuildPlanningCommandShapes()
    try testRigContracts(fixturesDir: fixturesDir, decoder: decoder)
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

    let source = try cliSourceContent()
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
