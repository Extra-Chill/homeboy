import Foundation

// MARK: - Cook composer
//
// Compiled together with the real `Homeboy/Views/Orchestration/CookComposerModel.swift`
// (Foundation-only), so these tests exercise production argument building,
// preview decoding, and model annotation rather than a copied mirror.

private enum ContractTestError: LocalizedError {
    case assertion(String)

    var errorDescription: String? {
        if case .assertion(let message) = self { return message }
        return nil
    }
}

func runCookComposerContractTests(fixturesDir: String) throws {
    try testCookComposerArguments()
    try testCookComposerPreviewInvalidation()
    try testCookComposerPreviewDecoding(fixturesDir: fixturesDir)
    try testCookComposerModelOptions(fixturesDir: fixturesDir)
}

private func composerForm() -> CookComposerForm {
    var form = CookComposerForm()
    form.repo = " homeboy "
    form.taskURL = "https://github.com/Extra-Chill/homeboy/issues/15024"
    form.goal = "Ship it"
    form.prompt = "Multi-line\nprompt with 'quotes'"
    form.verifyGates = ["cargo fmt --all -- --check", "  ", "cargo test -p homeboy-cli"]
    return form
}

private func testCookComposerArguments() throws {
    var form = composerForm()
    guard form.isValid else { throw ContractTestError.assertion("filled form must be valid") }

    let base = form.buildArguments(includePreview: false)
    let expectedBase = [
        "--repo", "homeboy",
        "--task-url", "https://github.com/Extra-Chill/homeboy/issues/15024",
        "--goal", "Ship it",
        "--prompt", "-",
        "--verify", "cargo fmt --all -- --check",
        "--verify", "cargo test -p homeboy-cli",
        "--draft-pr",
    ]
    guard base == expectedBase else {
        throw ContractTestError.assertion("default arguments mismatch: \(base)")
    }
    guard !base.contains(where: { $0.contains("quotes") }) else {
        throw ContractTestError.assertion("prompt text must be piped on stdin, never passed as an argument")
    }

    form.modelID = "anthropic/claude-sonnet-5"
    form.placement = .local
    form.draftPR = false
    let full = form.buildArguments(includePreview: true)
    guard full.suffix(6) == ["--model", "anthropic/claude-sonnet-5", "--acknowledge-model-override", "--placement", "local", "--preview"] else {
        throw ContractTestError.assertion("model/placement/preview arguments mismatch: \(full)")
    }
    guard !full.contains("--draft-pr") else {
        throw ContractTestError.assertion("--draft-pr must be omitted when the toggle is off")
    }

    var empty = CookComposerForm()
    empty.verifyGates = ["   "]
    let errors = empty.validationErrors
    guard Set(errors.keys) == [.repo, .taskURL, .goal, .prompt, .verify] else {
        throw ContractTestError.assertion("empty form must flag every required field: \(errors.keys)")
    }
    print("[PASS] Cook composer argument building and validation")
}

private func testCookComposerPreviewInvalidation() throws {
    var form = composerForm()
    let previewed = form.buildArguments(includePreview: false)
    guard form.matchesPreviewedArguments(previewed) else {
        throw ContractTestError.assertion("an unchanged form must match its preview")
    }
    guard !form.matchesPreviewedArguments(nil) else {
        throw ContractTestError.assertion("no successful preview must never allow submit")
    }
    form.verifyGates.append("swift test")
    guard !form.matchesPreviewedArguments(previewed) else {
        throw ContractTestError.assertion("editing a gate must invalidate the preview")
    }
    print("[PASS] Cook composer preview invalidation")
}

private func testCookComposerPreviewDecoding(fixturesDir: String) throws {
    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/agent-task-cook-preview.json"))
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let envelope = try decoder.decode(CookPreviewEnvelope.self, from: data)
    guard envelope.success,
          let preview = envelope.data,
          preview.schema == "homeboy/agent-task-cook-preview/v1",
          preview.mutates == false else {
        throw ContractTestError.assertion("preview fixture must decode as a non-mutating cook preview")
    }
    guard let replay = preview.replayArgv, Array(replay.prefix(3)) == ["homeboy", "agent-task", "cook"] else {
        throw ContractTestError.assertion("preview must carry a replayable cook argv")
    }
    guard let resolved = preview.resolved, resolved.provider?.backend == "opencode" else {
        throw ContractTestError.assertion("preview must resolve the opencode provider")
    }
    guard resolved.gates?.total == 1 else {
        throw ContractTestError.assertion("preview fixture declares exactly one gate, got \(String(describing: resolved.gates?.total))")
    }
    guard resolved.placement?.selected?.isEmpty == false else {
        throw ContractTestError.assertion("preview must report the selected placement")
    }
    print("[PASS] Cook preview envelope decoding")
}

private func testCookComposerModelOptions(fixturesDir: String) throws {
    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/agent-task-capacity.json"))
    guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any],
          let report = root["data"] as? [String: Any],
          let routes = report["routes"] as? [[String: Any]] else {
        throw ContractTestError.assertion("capacity fixture must contain data.routes")
    }
    let composerRoutes = routes.map { route -> CookComposerCapacityRoute in
        let capacity = route["capacity"] as? [String: Any] ?? [:]
        let exhausted = capacity["state"] as? String == "exhausted"
        let headline: String
        if exhausted {
            headline = "exhausted"
        } else if let remaining = capacity["remaining"] as? NSNumber {
            headline = "\(remaining)% remaining"
        } else {
            headline = "capacity not published"
        }
        return CookComposerCapacityRoute(
            models: route["models"] as? [String] ?? [],
            headline: headline,
            isExhausted: exhausted
        )
    }
    let options = CookComposerModelOption.options(from: composerRoutes)
    guard options.count == Set(options.map(\.id)).count, !options.isEmpty else {
        throw ContractTestError.assertion("model options must be non-empty and unique")
    }
    guard let openai = options.first(where: { $0.id.hasPrefix("openai/") }), openai.isExhausted else {
        throw ContractTestError.assertion("the exhausted openai route must mark its model exhausted")
    }
    guard let anthropic = options.first(where: { $0.id.hasPrefix("anthropic/") }),
          !anthropic.isExhausted,
          anthropic.displayName.hasSuffix("% remaining") else {
        throw ContractTestError.assertion("the available anthropic model must carry its remaining-capacity headline")
    }
    print("[PASS] Cook composer capacity-annotated model options")
}
