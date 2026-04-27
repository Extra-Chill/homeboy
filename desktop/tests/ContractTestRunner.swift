import Foundation

// MARK: - Aggregate Runner

func runTests(testDir: String) throws {
    let fixturesDir = "\(testDir)/fixtures"
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase

    print("Contract Tests")
    print("==============")
    print("Fixtures: \(fixturesDir)")
    print("")

    try runCLIBridgeModelDecodingContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)
    try runWorkspaceStatusConfigContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)
    try runFileLogDBContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)
    try runRigBenchReleaseUndoContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)
    try runStackGitAPIAuthContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)
    try runQualityAuditReviewTriageContractTests(testDir: testDir, fixturesDir: fixturesDir, decoder: decoder)

    print("")
    print("All contract tests passed")
}

@main
enum ContractTestRunner {
    static func main() {
        let args = CommandLine.arguments
        guard args.count > 1 else {
            print("Usage: ContractTestRunner <test-dir>")
            exit(1)
        }

        do {
            try runTests(testDir: args[1])
        } catch {
            print("[FAIL] Contract test failed: \(error.localizedDescription)")
            exit(1)
        }
    }
}
