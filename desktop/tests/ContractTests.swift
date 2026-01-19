#!/usr/bin/env swift

import Foundation

// MARK: - CLI Response Types (mirror DeployerViewModel.swift)

struct CLIResponse<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
    let error: CLIErrorResponse?
}

struct CLIErrorResponse: Decodable {
    let code: String
    let message: String
}

struct CLIDeploymentResult: Decodable {
    let results: [CLIComponentResult]
    let summary: CLIDeploymentSummary
}

struct CLIComponentResult: Decodable {
    let id: String
    let status: String
    let localVersion: String?
    let remoteVersion: String?
    let componentStatus: String?
    let error: String?
    let artifactPath: String?
    let remotePath: String?
}

struct CLIDeploymentSummary: Decodable {
    let succeeded: Int
    let failed: Int
    let skipped: Int
    let total: Int
}

// MARK: - Component List Types (mirror HomeboyCLI.swift)

struct ComponentListOutput: Decodable {
    let command: String
    let components: [ComponentListItemCLI]?
}

struct ComponentListItemCLI: Decodable {
    let id: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
}

// MARK: - Database Output Types (mirror HomeboyCLI.swift)

struct DbOutput: Decodable {
    let command: String
    let projectId: String
    let exitCode: Int32?
    let success: Bool?
    let stdout: String?
    let stderr: String?
    let tables: [String]?
    let table: String?
    let sql: String?
}

struct WPTable: Decodable {
    let Name: String
    let Rows: String?
    let Engine: String?
}

// MARK: - Test Runner

func runTests(testDir: String) throws {
    let fixturesDir = "\(testDir)/fixtures"
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase

    print("Contract Tests")
    print("==============")
    print("Fixtures: \(fixturesDir)")
    print("")

    // Test 1: deploy-dry-run.json parsing
    try testDeployDryRun(fixturesDir: fixturesDir, decoder: decoder)

    // Test 2: component-list.json parsing
    try testComponentList(fixturesDir: fixturesDir, decoder: decoder)

    // Test 3: db-describe.json parsing
    try testDbDescribe(fixturesDir: fixturesDir, decoder: decoder)

    print("")
    print("All contract tests passed")
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

    guard let components = output.components else {
        throw NSError(domain: "ContractTest", code: 13,
            userInfo: [NSLocalizedDescriptionKey: "component-list.json: components field is nil"])
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

// MARK: - Entry Point

let args = CommandLine.arguments
guard args.count > 1 else {
    print("Usage: ContractTests.swift <test-dir>")
    exit(1)
}

do {
    try runTests(testDir: args[1])
} catch {
    print("[FAIL] Contract test failed: \(error.localizedDescription)")
    exit(1)
}
