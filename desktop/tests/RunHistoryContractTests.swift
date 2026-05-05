import Foundation

// MARK: - run history

func runHistoryContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testRunHistoryFixtures(fixturesDir: fixturesDir, decoder: decoder)
    try testRunHistoryWrapperCommandShapes()
}

func testRunHistoryFixtures(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: run history command contracts")
    print("-----------------------------------")

    let listData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runs-list.json"))
    let listResult = try decoder.decode(CLIResponse<RunsListOutputTest>.self, from: listData)
    guard listResult.success, let runs = listResult.data?.runs, runs.count == 2 else {
        throw NSError(domain: "ContractTest", code: 130,
            userInfo: [NSLocalizedDescriptionKey: "runs-list.json did not decode two runs"])
    }
    guard runs[0].id == "run-bench-001", runs[0].componentId == "homeboy-desktop", runs[1].statusNote == "owner_process_not_running" else {
        throw NSError(domain: "ContractTest", code: 131,
            userInfo: [NSLocalizedDescriptionKey: "runs-list.json summary fields did not decode"])
    }
    print("[PASS] Runs list summaries decode")

    let showData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runs-show.json"))
    let showResult = try decoder.decode(CLIResponse<RunsShowOutputTest>.self, from: showData)
    guard showResult.success, let run = showResult.data?.run else {
        throw NSError(domain: "ContractTest", code: 132,
            userInfo: [NSLocalizedDescriptionKey: "runs-show.json did not decode run detail"])
    }
    guard run.id == "run-bench-001", run.homeboyVersion == "0.31.0", run.artifacts.count == 1 else {
        throw NSError(domain: "ContractTest", code: 133,
            userInfo: [NSLocalizedDescriptionKey: "runs-show.json detail fields did not decode"])
    }
    guard case .object(let metadata) = run.metadata,
          case .array(let scenarios) = metadata["scenario_metrics"],
          scenarios.count == 1 else {
        throw NSError(domain: "ContractTest", code: 134,
            userInfo: [NSLocalizedDescriptionKey: "runs-show.json metadata did not preserve arbitrary JSON"])
    }
    print("[PASS] Runs show detail and metadata decode")

    let artifactsData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runs-artifacts.json"))
    let artifactsResult = try decoder.decode(CLIResponse<RunsArtifactsOutputTest>.self, from: artifactsData)
    guard artifactsResult.success, let artifacts = artifactsResult.data?.artifacts, artifacts.count == 2 else {
        throw NSError(domain: "ContractTest", code: 135,
            userInfo: [NSLocalizedDescriptionKey: "runs-artifacts.json did not decode artifacts"])
    }
    guard artifacts[0].artifactType == "file", artifacts[1].artifactType == "url", artifacts[1].url == "https://example.test/report" else {
        throw NSError(domain: "ContractTest", code: 136,
            userInfo: [NSLocalizedDescriptionKey: "runs-artifacts.json file/url artifacts did not decode"])
    }
    print("[PASS] Runs artifacts file and URL records decode")

    let findingsData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runs-findings.json"))
    let findingsResult = try decoder.decode(CLIResponse<RunsFindingsOutputTest>.self, from: findingsData)
    guard findingsResult.success, let finding = findingsResult.data?.findings.first else {
        throw NSError(domain: "ContractTest", code: 137,
            userInfo: [NSLocalizedDescriptionKey: "runs-findings.json did not decode findings"])
    }
    guard finding.tool == "lint", finding.fixable == true,
          case .object(let metadata) = finding.metadataJson,
          case .string("lint-findings") = metadata["source_sidecar"] else {
        throw NSError(domain: "ContractTest", code: 138,
            userInfo: [NSLocalizedDescriptionKey: "runs-findings.json finding fields did not decode"])
    }
    print("[PASS] Runs findings decode")

    let compareData = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runs-compare.json"))
    let compareResult = try decoder.decode(CLIResponse<RunsCompareOutputTest>.self, from: compareData)
    guard compareResult.success, let compare = compareResult.data, compare.command == "runs.compare" else {
        throw NSError(domain: "ContractTest", code: 139,
            userInfo: [NSLocalizedDescriptionKey: "runs-compare.json did not decode compare output"])
    }
    guard compare.rows.count == 2, compare.rows[0].metrics["total_elapsed_ms"] == 1200.5 else {
        throw NSError(domain: "ContractTest", code: 140,
            userInfo: [NSLocalizedDescriptionKey: "runs-compare.json compare rows did not decode"])
    }
    print("[PASS] Runs compare JSON decodes")
    print("")
}

func testRunHistoryWrapperCommandShapes() throws {
    print("Test: run history wrapper command shapes")
    print("----------------------------------------")

    let source = try cliSourceContent()

    try assertContains(source, #"["runs", "list"]"#,
        message: "runsList wrapper uses homeboy runs list")
    try assertContains(source, #"["runs", "show", id]"#,
        message: "runsShow wrapper uses homeboy runs show")
    try assertContains(source, #"["runs", "artifacts", id]"#,
        message: "runsArtifacts wrapper uses homeboy runs artifacts")
    try assertContains(source, #"["runs", "findings", id]"#,
        message: "runsFindings wrapper uses homeboy runs findings")
    try assertContains(source, #"["runs", "compare", "--format", "json", "--kind", filter.kind]"#,
        message: "runsCompare wrapper forces JSON output")
    try assertContains(source, #"args.append(contentsOf: ["--component", componentId])"#,
        message: "run history wrappers support --component")
    try assertDoesNotContain(source, #"runs", "export"#,
        message: "Desktop run history wrappers do not expose bundle export")
    try assertDoesNotContain(source, #"runs", "import"#,
        message: "Desktop run history wrappers do not expose bundle import")

    print("")
}
