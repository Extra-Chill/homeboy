import Foundation

// MARK: - quality/review

func runQualityAuditReviewContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testQualityWorkspaceCommandShapes(testDir: testDir)
    try testCurrentCLICommandSurface(testDir: testDir)
}

func testQualityWorkspaceCommandShapes(testDir: String) throws {
    print("Test: Quality workspace command shapes")
    print("--------------------------------------")

    let sourceURL = URL(fileURLWithPath: "Homeboy/Core/CLI")
    let contentViewURL = URL(fileURLWithPath: "Homeboy/App/ContentView.swift")
    let qualityViewURL = URL(fileURLWithPath: "Homeboy/Extensions/Quality/Views/QualityView.swift")
    let qualityViewModelURL = URL(fileURLWithPath: "Homeboy/Extensions/Quality/QualityViewModel.swift")

    let source = try cliSourceContent(at: sourceURL)
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
    try requireContains(source, #"HomeboyCommandRegistry.arguments("review", appending: [componentId, "--summary"])"#, "Quality review uses current review --summary command", code: 102)
    try requireContains(source, #"args.append(contentsOf: ["--changed-since", changedSince])"#, "Quality scope supports changed-since", code: 104)
    try requireContains(source, #"args.append("--changed-only")"#, "Quality scope supports changed-only", code: 105)
    try requireContains(source, #"args.append(contentsOf: ["--path", path])"#, "Quality commands support component path override", code: 106)
    try requireContains(source, "executeCommandWithOutputFile", "Quality decoding reads --output JSON envelopes", code: 107)
    try requireContains(qualityView, "Run Review", "Quality UI exposes review entry point", code: 108)
    try requireContains(qualityView, "Findings:", "Quality UI renders stage finding summaries", code: 110)
    try requireContains(qualityViewModel, "HomeboyCLI.shared.qualityStage", "Quality view model exposes stage deep-dives", code: 111)

    try requireNotContains(qualityView, "--fix", "Quality UI does not expose lint --fix", code: 116)
    try requireNotContains(qualityView, "--write", "Quality UI does not expose write actions", code: 117)
    try requireNotContains(source, "--baseline", "Quality helpers do not expose baseline writes", code: 118)
    try requireNotContains(source, "--ratchet", "Quality helpers do not expose ratchet writes", code: 119)

    print("")
}

func testCurrentCLICommandSurface(testDir: String) throws {
    print("Test: current CLI command surface")
    print("---------------------------------")

    let source = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Core/CLI")
    let content = try cliSourceContent(at: source)

    let requiredShapes = [
        "HomeboyCommandRegistry.arguments(\"review lint\", appending: [componentId])",
        "HomeboyCommandRegistry.arguments(\"review test\", appending: [componentId])",
        "\"refactor rename\"",
    ]

    for shape in requiredShapes {
        guard content.contains(shape) else {
            throw NSError(domain: "ContractTest", code: 71,
                userInfo: [NSLocalizedDescriptionKey: "Expected CLI construction shape is missing: \(shape)"])
        }
    }
    print("[PASS] Quality stages route through supported review subcommands")

    print("")
}
