import Foundation

// MARK: - file/log/db

func runFileLogDBContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testDbDescribe(fixturesDir: fixturesDir, decoder: decoder)
    try testRemoteLogViewerPinCommandShape(testDir: testDir)
    try testRemoteFileEditorPinCommandShape(testDir: testDir)
    try testRemoteFileEditorModelAndPathShape(testDir: testDir)
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
        .appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI+ProjectPinCommands.swift")

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

func testRemoteFileEditorPinCommandShape(testDir: String) throws {
    print("Test: Remote File Editor pin command shape")
    print("------------------------------------------")

    let viewModelPath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Extensions/RemoteFileEditor/RemoteFileEditorViewModel.swift")
    let cliPath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI+ProjectPinCommands.swift")

    let viewModelSource = try String(contentsOf: viewModelPath, encoding: .utf8)
    let cliSource = try String(contentsOf: cliPath, encoding: .utf8)
    let currentAddCommand = #"["project", "pin", "add", projectId, path, "--type", "file"]"#
    let currentRemoveCommand = #"["project", "pin", "remove", projectId, path, "--type", "file"]"#

    guard cliSource.contains(currentAddCommand) else {
        throw NSError(domain: "ContractTest", code: 74,
            userInfo: [NSLocalizedDescriptionKey: "File CLI wrapper pin command does not use current homeboy project pin add syntax"])
    }
    print("[PASS] File pin command uses current add syntax")

    guard cliSource.contains(currentRemoveCommand) else {
        throw NSError(domain: "ContractTest", code: 75,
            userInfo: [NSLocalizedDescriptionKey: "File CLI wrapper unpin command does not use current homeboy project pin remove syntax"])
    }
    print("[PASS] File unpin command uses current remove syntax")

    guard !viewModelSource.contains("[\"project\", \"pin\"") else {
        throw NSError(domain: "ContractTest", code: 76,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditorViewModel still builds raw project pin commands instead of using HomeboyCLI wrappers"])
    }
    print("[PASS] View model uses typed HomeboyCLI file pin wrappers")

    print("")
}

func testRemoteFileEditorModelAndPathShape(testDir: String) throws {
    print("Test: Remote File Editor model and path shape")
    print("---------------------------------------------")

    let root = URL(fileURLWithPath: testDir).deletingLastPathComponent()
    let modelPath = root.appendingPathComponent("Homeboy/Models/RemoteFileModels.swift")
    let viewModelPath = root.appendingPathComponent("Homeboy/Extensions/RemoteFileEditor/RemoteFileEditorViewModel.swift")
    let viewPath = root.appendingPathComponent("Homeboy/Extensions/RemoteFileEditor/Views/RemoteFileEditorView.swift")

    let modelSource = try String(contentsOf: modelPath, encoding: .utf8)
    let viewModelSource = try String(contentsOf: viewModelPath, encoding: .utf8)
    let viewSource = try String(contentsOf: viewPath, encoding: .utf8)

    guard modelSource.contains("struct OpenFile: PinnableTabItem, Equatable") else {
        throw NSError(domain: "ContractTest", code: 77,
            userInfo: [NSLocalizedDescriptionKey: "OpenFile model is not defined in Homeboy/Models/RemoteFileModels.swift"])
    }
    print("[PASS] OpenFile lives in shared model file")

    guard !viewModelSource.contains("struct OpenFile") else {
        throw NSError(domain: "ContractTest", code: 78,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditorViewModel still owns the OpenFile model"])
    }
    print("[PASS] View model no longer owns OpenFile")

    guard viewSource.contains("viewModel.openFileFromBrowser(path: path)") else {
        throw NSError(domain: "ContractTest", code: 79,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditorView still normalizes browser paths locally"])
    }
    print("[PASS] Browser path normalization is delegated to the view model")

    guard viewModelSource.contains("private func relativeFilePath(for path: String) -> String") else {
        throw NSError(domain: "ContractTest", code: 80,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditorViewModel does not expose a centralized relative path helper"])
    }
    print("[PASS] View model centralizes relative path conversion")

    guard viewModelSource.contains("var canRefreshSelectedFile: Bool")
        && viewModelSource.contains("return !selectedFile.hasUnsavedChanges && !isLoading && !isSaving") else {
        throw NSError(domain: "ContractTest", code: 83,
            userInfo: [NSLocalizedDescriptionKey: "Remote File Editor does not disable refresh while the selected file has unsaved changes"])
    }
    print("[PASS] Refresh availability excludes unsaved selected files")

    guard viewModelSource.contains("guard !openFiles[index].hasUnsavedChanges else { return }") else {
        throw NSError(domain: "ContractTest", code: 84,
            userInfo: [NSLocalizedDescriptionKey: "Remote File Editor refresh path can still overwrite unsaved selected file content"])
    }
    print("[PASS] Fetch path refuses to overwrite unsaved selected file content")

    guard viewSource.contains(".disabled(!viewModel.canRefreshSelectedFile)") else {
        throw NSError(domain: "ContractTest", code: 85,
            userInfo: [NSLocalizedDescriptionKey: "Remote File Editor refresh button is not bound to refresh safety state"])
    }
    print("[PASS] Refresh button is bound to refresh safety state")

    guard viewModelSource.contains("openFiles[index].fileSize = oldFile.fileSize") else {
        throw NSError(domain: "ContractTest", code: 81,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditor rename path does not preserve fileSize"])
    }
    print("[PASS] Rename preserves file size metadata")

    guard !viewModelSource.contains("dropFirst(base.count") && !viewSource.contains("dropFirst(basePath.count") else {
        throw NSError(domain: "ContractTest", code: 82,
            userInfo: [NSLocalizedDescriptionKey: "Remote File Editor still contains duplicated prefix-drop path conversion"])
    }
    print("[PASS] Duplicated prefix-drop path conversion is absent")

    print("")
}
