#!/usr/bin/env swift

import Foundation

let args = CommandLine.arguments
guard args.count > 1 else {
    print("Usage: ContractTests.swift <test-dir>")
    exit(1)
}

let scriptURL = URL(fileURLWithPath: args[0])
let testsDir = scriptURL.deletingLastPathComponent()
let repoRoot = testsDir.deletingLastPathComponent()
let binaryURL = FileManager.default.temporaryDirectory
    .appendingPathComponent("homeboy-desktop-contract-tests-\(UUID().uuidString)")
let aggregateFiles = [
    "ContractTestSupport.swift",
    "CLIBridgeModelContractTests.swift",
    "WorkspaceStatusConfigContractTests.swift",
    "FileLogDBContractTests.swift",
    "RigBenchReleaseUndoContractTests.swift",
    "RunHistoryContractTests.swift",
    "StackGitAPIAuthContractTests.swift",
    "QualityAuditReviewTriageContractTests.swift",
    "ContractTestRunner.swift",

].map { testsDir.appendingPathComponent($0).path }

let compile = Process()
compile.executableURL = URL(fileURLWithPath: "/usr/bin/env")
compile.arguments = ["swiftc"] + aggregateFiles + ["-o", binaryURL.path]
compile.currentDirectoryURL = repoRoot

try compile.run()
compile.waitUntilExit()
guard compile.terminationStatus == 0 else {
    exit(compile.terminationStatus)
}

let run = Process()
run.executableURL = binaryURL
run.arguments = [args[1]]
run.currentDirectoryURL = repoRoot

try run.run()
run.waitUntilExit()
try? FileManager.default.removeItem(at: binaryURL)
exit(run.terminationStatus)
