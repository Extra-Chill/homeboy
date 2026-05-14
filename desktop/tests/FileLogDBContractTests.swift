import Foundation

// MARK: - file/log/db

func runFileLogDBContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testDbDescribe(fixturesDir: fixturesDir, decoder: decoder)
    try testRemoteLogViewerPinCommandShape(testDir: testDir)
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

func testRemoteLogViewerPinCommandShape(testDir: String) throws {
    print("Test: Remote Log Viewer pin command shape")
    print("-----------------------------------------")

    let viewModelPath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Extensions/RemoteLogViewer/RemoteLogViewerViewModel.swift")
    let cliPath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI+LogCommands.swift")

    let viewModelSource = try String(contentsOf: viewModelPath, encoding: .utf8)
    let cliSource = try String(contentsOf: cliPath, encoding: .utf8)
    let currentPinCommand = #"["project", "pin", "add", "--type", "log", "--tail", String(tailLines), projectId, path]"#
    let currentTailUpdateCommand = #"["project", "pin", "add", "--type", "log", "--tail", String(tailLines), projectId, path]"#
    let oldPositionalFirstCommand = #"["project", "pin", "add", projectId, log.path, "--type", "log", "--tail"#

    guard cliSource.contains(currentPinCommand) else {
        throw NSError(domain: "ContractTest", code: 70,
            userInfo: [NSLocalizedDescriptionKey: "Log CLI wrapper pin command does not use current homeboy project pin add syntax"])
    }
    print("[PASS] Pin command puts --type/--tail before project/path")

    guard cliSource.contains(currentTailUpdateCommand) else {
        throw NSError(domain: "ContractTest", code: 71,
            userInfo: [NSLocalizedDescriptionKey: "Log CLI wrapper tail update command does not preserve current pin syntax"])
    }
    print("[PASS] Tail-line update command preserves --tail before project/path")

    guard !viewModelSource.contains("[\"project\", \"pin\"") else {
        throw NSError(domain: "ContractTest", code: 72,
            userInfo: [NSLocalizedDescriptionKey: "RemoteLogViewerViewModel still builds raw project pin commands instead of using HomeboyCLI wrappers"])
    }
    print("[PASS] View model uses typed HomeboyCLI log pin wrappers")

    guard !cliSource.contains(oldPositionalFirstCommand) else {
        throw NSError(domain: "ContractTest", code: 73,
            userInfo: [NSLocalizedDescriptionKey: "RemoteLogViewer still contains old positional-first project pin add syntax"])
    }
    print("[PASS] Old positional-first log pin syntax is absent")

    print("")
}
