import Foundation

// MARK: - file

func runFileContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testRemoteFileEditorPinCommandShape(testDir: testDir)
    try testRemoteFileEditorModelAndPathShape(testDir: testDir)
    try testRemoteFilePinnedReloadReconciliation(testDir: testDir)
    try testRemoteFileBrowserStaleRequestGuard(testDir: testDir)
    try testPinnedTabClosePreservesVisibleStateUntilUnpinSucceeds(testDir: testDir)
}

func testRemoteFilePinnedReloadReconciliation(testDir: String) throws {
    print("Test: Remote File pinned reload reconciliation")
    print("---------------------------------------------")

    let root = URL(fileURLWithPath: testDir).deletingLastPathComponent()
    let fileViewModelPath = root.appendingPathComponent("Homeboy/Extensions/RemoteFileEditor/RemoteFileEditorViewModel.swift")

    let fileSource = try String(contentsOf: fileViewModelPath, encoding: .utf8)

    guard fileSource.contains("reconcilePinnedFiles()") else {
        throw NSError(domain: "ContractTest", code: 89,
            userInfo: [NSLocalizedDescriptionKey: "RemoteFileEditorViewModel does not reconcile pinned files on config reload"])
    }
    print("[PASS] Remote File Editor uses a pinned-file reconcile path")

    guard fileSource.contains("openFiles[index].isPinned = pinnedByPath[openFiles[index].path] != nil") else {
        throw NSError(domain: "ContractTest", code: 91,
            userInfo: [NSLocalizedDescriptionKey: "Remote File Editor reconcile path does not update existing file pin state"])
    }
    print("[PASS] Existing file tabs keep their tab identity while pin state changes")

    guard fileSource.contains("openFiles.append(OpenFile(from: pinned))") else {
        throw NSError(domain: "ContractTest", code: 93,
            userInfo: [NSLocalizedDescriptionKey: "Pinned reload reconciliation does not append newly pinned tabs"])
    }
    print("[PASS] Newly pinned files are appended without replacing temporary tabs")

    guard fileSource.contains("openFiles = config.remoteFiles.pinnedFiles.map") else {
        throw NSError(domain: "ContractTest", code: 94,
            userInfo: [NSLocalizedDescriptionKey: "Project-switch pinned tab reset loaders are missing"])
    }
    print("[PASS] Project-switch full reset loaders remain available")

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
    let currentAddCommand = #"["project", "pin", "add", projectId, path]"#
    let currentRemoveCommand = #"["project", "pin", "remove", projectId, path]"#

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

func testRemoteFileBrowserStaleRequestGuard(testDir: String) throws {
    print("Test: Remote File Browser stale request guard")
    print("---------------------------------------------")

    let browserPath = URL(fileURLWithPath: testDir)
        .deletingLastPathComponent()
        .appendingPathComponent("Homeboy/Core/SSH/RemoteFileBrowser.swift")
    let browserSource = try String(contentsOf: browserPath, encoding: .utf8)
    let goToPathSource = try sourceSlice(
        browserSource,
        from: "func goToPath(_ path: String) async {",
        to: "    /// Navigate to parent directory"
    )

    try assertContains(
        browserSource,
        "private var requestGeneration = 0",
        message: "RemoteFileBrowser does not track file-list request generations"
    )
    print("[PASS] Browser tracks request generations")

    try assertContains(
        browserSource,
        "private var activeListRequest: FileListRequest?",
        message: "RemoteFileBrowser does not retain the active file-list request identity"
    )
    print("[PASS] Browser retains active request identity")

    try assertContains(
        browserSource,
        "activeListRequest = nil",
        message: "RemoteFileBrowser reconnect does not invalidate in-flight file-list requests"
    )
    print("[PASS] Reconnect invalidates in-flight requests")

    try assertContains(
        goToPathSource,
        "let request = FileListRequest(generation: requestGeneration, projectId: projectId, path: path)",
        message: "goToPath does not capture generation, project, and path before awaiting file list"
    )
    print("[PASS] Navigation captures generation/project/path")

    try assertContains(
        goToPathSource,
        "guard activeListRequest == request else { return }\n            currentPath = path",
        message: "goToPath can still apply stale file-list entries after a newer request wins"
    )
    print("[PASS] Stale success results are discarded before mutating entries")

    try assertContains(
        goToPathSource,
        "guard activeListRequest == request else { return }\n            self.error = error.toDisplayableError",
        message: "goToPath can still show stale errors after a newer request wins"
    )
    print("[PASS] Stale errors are discarded")

    try assertContains(
        goToPathSource,
        "if activeListRequest == request {\n            isLoading = false\n        }",
        message: "goToPath can still clear loading state for a newer in-flight request"
    )
    print("[PASS] Loading state is cleared only by the active request")

    print("")
}

func testPinnedTabClosePreservesVisibleStateUntilUnpinSucceeds(testDir: String) throws {
    print("Test: Pinned tab close preserves visible state")
    print("------------------------------------------------")

    let root = URL(fileURLWithPath: testDir).deletingLastPathComponent()
    let fileViewModelPath = root.appendingPathComponent("Homeboy/Extensions/RemoteFileEditor/RemoteFileEditorViewModel.swift")

    let fileSource = try String(contentsOf: fileViewModelPath, encoding: .utf8)

    guard fileSource.contains("_ = try await cli.filePinRemove(projectId: projectId, path: file.path)\n                    closeOpenFileTab(id)") else {
        throw NSError(domain: "ContractTest", code: 86,
            userInfo: [NSLocalizedDescriptionKey: "Remote File close does not wait for pinned unpin before removing the tab"])
    }
    print("[PASS] Pinned file close removes the tab only after unpin succeeds")

    guard fileSource.contains("Failed to unpin file:") else {
        throw NSError(domain: "ContractTest", code: 87,
            userInfo: [NSLocalizedDescriptionKey: "Remote File close does not surface pinned unpin failures"])
    }
    print("[PASS] Pinned file close surfaces unpin failure")

    print("")
}
