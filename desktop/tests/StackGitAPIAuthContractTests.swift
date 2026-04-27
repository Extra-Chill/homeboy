import Foundation

// MARK: - stack/git/api/auth

func runStackGitAPIAuthContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testStackContracts(fixturesDir: fixturesDir, decoder: decoder)
    try testGitStatus(fixturesDir: fixturesDir, decoder: decoder)
    try testGitWorkspaceCommandShapes(testDir: testDir)
    try testAPIAuthContracts(decoder: decoder)
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

    let source = try cliSourceContent()

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
