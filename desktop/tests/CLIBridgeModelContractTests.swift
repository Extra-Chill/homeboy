import Foundation

// MARK: - CLI bridge / model decoding

func runCLIBridgeModelDecodingContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testDeployDryRun(fixturesDir: fixturesDir, decoder: decoder)
    try testExtensionList(fixturesDir: fixturesDir, decoder: decoder)
    try testDaemonEnsureRunning(fixturesDir: fixturesDir, decoder: decoder)
    try testHomeboyCLIHelperCommandShapes()
    try testMutationCommandShapes(testDir: testDir)
}

/// Mirrors `DaemonEnsureRunningOutput` in Core/API/HomeboyDaemonLocator.swift.
private struct DaemonEnsureRunningOutput: Decodable {
    let address: String?
}

/// The control-plane locator discovers the daemon through
/// `homeboy daemon ensure-running`, which returns the live daemon or starts
/// one. Its address is at `data.address` (not `data.daemon.address`, the
/// `daemon status` projection).
func testDaemonEnsureRunning(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: daemon-ensure-running.json")
    print("--------------------------------")
    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/daemon-ensure-running.json"))
    let result = try decoder.decode(CLIResponse<DaemonEnsureRunningOutput>.self, from: data)
    guard result.success, result.data?.address == "127.0.0.1:54321" else {
        throw NSError(domain: "ContractTest", code: 1,
            userInfo: [NSLocalizedDescriptionKey: "daemon-ensure-running.json: address not decoded from data.address"])
    }
    let locator = try String(contentsOfFile: "Homeboy/Core/API/HomeboyDaemonLocator.swift", encoding: .utf8)
    try assertContains(locator, #"HomeboyCommandRegistry.arguments("daemon ensure-running")"#,
        message: "locator must discover the daemon through daemon ensure-running")
    print("[PASS] Decoded daemon address from ensure-running data.address")
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

func testHomeboyCLIHelperCommandShapes() throws {
    print("Test: HomeboyCLI helper command shapes")
    print("--------------------------------------")

    let sourceURL = URL(fileURLWithPath: "Homeboy/Core/CLI")
    guard FileManager.default.fileExists(atPath: sourceURL.path) else {
        throw NSError(domain: "ContractTest", code: 80,
            userInfo: [NSLocalizedDescriptionKey: "Source file not found: \(sourceURL.path)"])
    }

    let source = try cliSourceContent(at: sourceURL)

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

    try requireContains("HomeboyCommandRegistry.arguments(\n            \"refactor rename\",\n            appending: [\"--from\", from, \"--to\", to, \"--component\", componentId]\n        )",
        "refactor rename uses current component flag shape", code: 81)
    try requireContains("[\"--file-type\", type]",
        "file find uses --file-type", code: 82)
    try requireContains("[\"--max-depth\", String(depth)]",
        "file search depth is converted before parser validation", code: 84)
    try requireContains("HomeboyCommandRegistry.arguments(\"status\", appending: [\"--full\"])",
        "workspace discovery uses homeboy status --full", code: 92)

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
    try requireNotContains("var args = [\"init\"]",
        "workspace discovery no longer uses deprecated homeboy init", code: 93)

    print("")
}

func testMutationCommandShapes(testDir: String) throws {
    print("Test: mutation command shapes")
    print("-----------------------------")

    let testURL = URL(fileURLWithPath: testDir)
    let repoRoot = testURL.lastPathComponent == "tests" ? testURL.deletingLastPathComponent() : testURL
    let cliURL = repoRoot.appendingPathComponent("Homeboy/Core/CLI")

    guard FileManager.default.fileExists(atPath: cliURL.path) else {
        throw NSError(domain: "ContractTest", code: 100,
            userInfo: [NSLocalizedDescriptionKey: "Homeboy CLI source directory not found at \(cliURL.path)"])
    }

    let source = try cliSourceContent(at: cliURL)
    let componentCreate = try sourceSlice(
        source,
        from: "func componentCreate(",
        to: "    func componentSet("
    )

    try assertContains(componentCreate, "\"--local-path\"", message: "component create must pass local path as an explicit flag")
    try assertContains(componentCreate, "\"--remote-path\"", message: "component create must pass remote path as an explicit flag")
    try assertContains(componentCreate, "\"--build-artifact\"", message: "component create keeps build artifact flag")
    try assertDoesNotContain(componentCreate, "\"component\", \"create\", name, localPath, remotePath", message: "component create must not use legacy positional name/localPath/remotePath")
    print("[PASS] component create uses explicit flags")
    print("")
}
