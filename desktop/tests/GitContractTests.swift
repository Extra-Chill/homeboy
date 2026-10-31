import Foundation

// MARK: - git

func runGitContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testGitStatus(fixturesDir: fixturesDir, decoder: decoder)
    try testGitWorkspaceCommandShapes(testDir: testDir)
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

func testGitWorkspaceCommandShapes(testDir: String) throws {
    print("Test: Git workspace command shapes")
    print("----------------------------------")

    let cliSource = try cliSourceContent()
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

    try requireContains(cliSource, #"HomeboyCommandRegistry.arguments("git status", appending: [componentId])"#,
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
