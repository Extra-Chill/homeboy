import Foundation

func runHomeboyCommandPathContractTests(testDir: String) throws {
    print("Test: Homeboy command path drift")
    print("--------------------------------")

    let repoRoot = URL(fileURLWithPath: testDir).deletingLastPathComponent()
    let fixtureURL = repoRoot.appendingPathComponent("tests/fixtures/homeboy-command-manifest.txt")
    let fixtureText = try String(contentsOf: fixtureURL, encoding: .utf8)
    let fixture = Set(fixtureText.split(whereSeparator: \.isNewline).map(String.init).filter { !$0.isEmpty })

    let missing = HomeboyCommandRegistry.invokedPaths.filter { !fixture.contains($0) }
    guard missing.isEmpty else {
        throw NSError(domain: "ContractTest", code: 200,
            userInfo: [NSLocalizedDescriptionKey: "Desktop invokes Homeboy command paths absent from the manifest fixture: \(missing.joined(separator: ", "))"])
    }
    print("[PASS] Registered command paths are in the manifest fixture")

    let homeboyDir = repoRoot.appendingPathComponent("Homeboy")
    let swiftFiles = try swiftSourceFiles(under: homeboyDir)
    var referenced: Set<String> = []
    let argumentPattern = try NSRegularExpression(pattern: #"HomeboyCommandRegistry\.arguments\(\s*"([^"]+)""#)
    let arrayPattern = try NSRegularExpression(pattern: #"(?<![A-Za-z0-9_])\[\s*"([a-z][a-z0-9-]*)"\s*(?:,|\])"#)
    let commandTokens = Set(fixture.map { $0.split(separator: " ").first.map(String.init) ?? $0 })
        .union(["audit", "lint", "test", "undo", "build", "changes", "validate", "observe", "doctor", "docs", "changelog", "issues", "list", "version"])
    let allowedArrays = [
        #"process.arguments = ["git", "-C", path, "remote", "get-url", "origin"]"#,
        #"let actions = ["lint", "test", "bench"]"#,
    ]

    for file in swiftFiles {
        let text = try String(contentsOf: file, encoding: .utf8)
        let nsText = text as NSString
        let fullRange = NSRange(location: 0, length: nsText.length)

        for match in argumentPattern.matches(in: text, range: fullRange) {
            let path = nsText.substring(with: match.range(at: 1))
            referenced.insert(path)
            guard HomeboyCommandRegistry.invokedPaths.contains(path) else {
                throw NSError(domain: "ContractTest", code: 201,
                    userInfo: [NSLocalizedDescriptionKey: "\(file.lastPathComponent) invokes unregistered path \(path)"])
            }
            guard fixture.contains(path) else {
                throw NSError(domain: "ContractTest", code: 202,
                    userInfo: [NSLocalizedDescriptionKey: "\(file.lastPathComponent) invokes \(path), which is absent from the manifest fixture"])
            }
        }

        if file.lastPathComponent == "HomeboyCommandRegistry.swift" {
            continue
        }

        for match in arrayPattern.matches(in: text, range: fullRange) {
            let token = nsText.substring(with: match.range(at: 1))
            guard commandTokens.contains(token) else { continue }
            let lineRange = nsText.lineRange(for: match.range)
            let line = nsText.substring(with: lineRange)
            if allowedArrays.contains(where: { line.contains($0) }) {
                continue
            }
            throw NSError(domain: "ContractTest", code: 203,
                userInfo: [NSLocalizedDescriptionKey: "\(file.path) has a raw Homeboy command array starting with \(token). Route it through HomeboyCommandRegistry."])
        }
    }

    let registered = Set(HomeboyCommandRegistry.invokedPaths)
    let unreferenced = registered.subtracting(referenced).sorted()
    guard unreferenced.isEmpty else {
        throw NSError(domain: "ContractTest", code: 204,
            userInfo: [NSLocalizedDescriptionKey: "Registered command paths have no call site: \(unreferenced.joined(separator: ", "))"])
    }
    print("[PASS] Every Desktop command path is registered and present in the fixture")
    print("")
}

private func swiftSourceFiles(under root: URL) throws -> [URL] {
    guard let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil) else {
        throw NSError(domain: "ContractTest", code: 205,
            userInfo: [NSLocalizedDescriptionKey: "Could not enumerate \(root.path)"])
    }
    return enumerator.compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }.sorted { $0.path < $1.path }
}
