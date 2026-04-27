import Foundation

// MARK: - workspace/status/config

func runWorkspaceStatusConfigContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testComponentList(fixturesDir: fixturesDir, decoder: decoder)
    try testComponentConfigurationFullDecode(fixturesDir: fixturesDir, decoder: decoder)
    try testComponentConfigurationMinimal(decoder: decoder)
    try testDisplayNameComputation()
    try testVersionTargetsParsing(fixturesDir: fixturesDir, decoder: decoder)
    try testCurrentComponentConfigurationFields(fixturesDir: fixturesDir, decoder: decoder)
    try testCurrentProjectConfigurationFields(decoder: decoder)
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

func testCurrentComponentConfigurationFields(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: current component configuration fields")
    print("--------------------------------------------")

    let fixture = URL(fileURLWithPath: "\(fixturesDir)/component.json")
    let data = try Data(contentsOf: fixture)
    let component = try decoder.decode(ComponentConfigurationTest.self, from: data)

    guard component.changelogNextSectionLabel == "Unreleased" else {
        throw NSError(domain: "ContractTest", code: 230,
            userInfo: [NSLocalizedDescriptionKey: "changelog_next_section_label did not decode"])
    }
    print("[PASS] changelog next section label decodes")

    guard component.changelogNextSectionAliases == ["Next", "Upcoming"] else {
        throw NSError(domain: "ContractTest", code: 231,
            userInfo: [NSLocalizedDescriptionKey: "changelog_next_section_aliases did not decode"])
    }
    print("[PASS] changelog next section aliases decode")

    guard component.extractCommand?.contains("unzip") == true,
          component.remoteOwner == "www-data:www-data",
          component.deployStrategy == "git" else {
        throw NSError(domain: "ContractTest", code: 232,
            userInfo: [NSLocalizedDescriptionKey: "deploy metadata fields did not decode"])
    }
    print("[PASS] deploy metadata fields decode")

    guard component.gitDeploy?.remote == "origin",
          component.gitDeploy?.branch == "main",
          component.gitDeploy?.postPull == ["composer install --no-dev"],
          component.gitDeploy?.tagPattern == "v{{version}}" else {
        throw NSError(domain: "ContractTest", code: 233,
            userInfo: [NSLocalizedDescriptionKey: "git_deploy did not decode"])
    }
    print("[PASS] git_deploy decodes")

    guard component.remoteUrl == "https://github.com/Extra-Chill/extrachill.git",
          component.autoCleanup == true,
          component.docsDir == "docs",
          component.docsDirs == ["docs", "guides"] else {
        throw NSError(domain: "ContractTest", code: 234,
            userInfo: [NSLocalizedDescriptionKey: "remote/docs cleanup fields did not decode"])
    }
    print("[PASS] remote/docs cleanup fields decode")

    guard component.scopes?.defaults?.include == ["src/**"],
          component.scopes?.defaults?.exclude == ["vendor/**"],
          component.scopes?.audit?.include == ["Homeboy/**"] else {
        throw NSError(domain: "ContractTest", code: 235,
            userInfo: [NSLocalizedDescriptionKey: "scopes did not decode"])
    }
    print("[PASS] scopes decode")

    let minimalGitDeploy = #"{"git_deploy":{}}"#.data(using: .utf8)!
    struct MinimalGitDeployWrapper: Decodable { let gitDeploy: GitDeployConfigTest }
    let wrapper = try decoder.decode(MinimalGitDeployWrapper.self, from: minimalGitDeploy)
    guard wrapper.gitDeploy.remote == "origin", wrapper.gitDeploy.branch == "main", wrapper.gitDeploy.postPull.isEmpty else {
        throw NSError(domain: "ContractTest", code: 236,
            userInfo: [NSLocalizedDescriptionKey: "git_deploy defaults did not decode"])
    }
    print("[PASS] git_deploy defaults decode")
    print("")
}

func testCurrentProjectConfigurationFields(decoder: JSONDecoder) throws {
    print("Test: current project configuration fields")
    print("------------------------------------------")

    let json = #"""
    {
        "domain": "example.test",
        "changelog_next_section_label": "Unreleased",
        "changelog_next_section_aliases": ["Next"],
        "components": [
            {"id": "data-machine", "local_path": "/Users/chubes/Developer/data-machine"}
        ],
        "component_overrides": {
            "data-machine": {
                "remote_path": "wp-content/plugins/data-machine",
                "extract_command": "unzip artifact.zip",
                "remote_owner": "www-data:www-data",
                "deploy_strategy": "git",
                "git_deploy": {"post_pull": ["composer install --no-dev"]},
                "hooks": {"post:deploy": ["wp cache flush"]},
                "scopes": {"lint": {"include": ["inc/**"], "exclude": []}},
                "cli_path": "studio wp"
            }
        },
        "services": ["nginx", "php8.4-fpm"]
    }
    """#.data(using: .utf8)!

    let project = try decoder.decode(ProjectConfigCLITest.self, from: json)
    guard project.changelogNextSectionLabel == "Unreleased",
          project.changelogNextSectionAliases == ["Next"] else {
        throw NSError(domain: "ContractTest", code: 240,
            userInfo: [NSLocalizedDescriptionKey: "project changelog fields did not decode"])
    }
    print("[PASS] Project changelog fields decode")

    guard project.components.first?.id == "data-machine",
          project.componentIds == ["data-machine"] else {
        throw NSError(domain: "ContractTest", code: 241,
            userInfo: [NSLocalizedDescriptionKey: "project components did not decode or derive componentIds"])
    }
    print("[PASS] Project components decode and derive componentIds")

    guard project.services == ["nginx", "php8.4-fpm"] else {
        throw NSError(domain: "ContractTest", code: 242,
            userInfo: [NSLocalizedDescriptionKey: "project services did not decode"])
    }
    print("[PASS] Project services decode")

    guard let override = project.componentOverrides["data-machine"],
          override.remotePath == "wp-content/plugins/data-machine",
          override.extractCommand == "unzip artifact.zip",
          override.remoteOwner == "www-data:www-data",
          override.deployStrategy == "git",
          override.gitDeploy?.remote == "origin",
          override.gitDeploy?.branch == "main",
          override.gitDeploy?.postPull == ["composer install --no-dev"],
          override.hooks["post:deploy"] == ["wp cache flush"],
          override.scopes?.lint?.include == ["inc/**"],
          override.cliPath == "studio wp" else {
        throw NSError(domain: "ContractTest", code: 243,
            userInfo: [NSLocalizedDescriptionKey: "project component_overrides did not decode"])
    }
    print("[PASS] Project component overrides decode")
    print("")
}
