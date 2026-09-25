import Foundation

// MARK: - control plane / capacity / runner status

func runControlPlaneContractTests(testDir: String, fixturesDir: String, decoder: JSONDecoder) throws {
    try testControlPlaneCapabilities(fixturesDir: fixturesDir, decoder: decoder)
    try testControlPlaneMissions(fixturesDir: fixturesDir, decoder: decoder)
    try testControlPlaneRuns(fixturesDir: fixturesDir, decoder: decoder)
    try testControlPlaneEvents(fixturesDir: fixturesDir, decoder: decoder)
    try testControlPlaneReview(fixturesDir: fixturesDir, decoder: decoder)
    try testAgentTaskCapacity(fixturesDir: fixturesDir, decoder: decoder)
    try testRunnerStatus(fixturesDir: fixturesDir, decoder: decoder)
    try testFormattingHelpers()
}

// MARK: - Envelope decode types (mirror Core/API/ControlPlaneModels.swift and ControlPlaneClient.swift)

private struct ControlPlaneEnvelopeTest<Resource: Decodable>: Decodable {
    let success: Bool
    let data: ControlPlaneEnvelopeDataTest<Resource>
}

private struct ControlPlaneEnvelopeDataTest<Resource: Decodable>: Decodable {
    let body: ControlPlaneBodyTest<Resource>
}

private struct ControlPlaneBodyTest<Resource: Decodable>: Decodable {
    let ok: Bool
    let resource: Resource?
}

private struct ControlPlaneCapabilitiesTest: Decodable {
    let resources: [String]
    let operations: [String]
}

private struct ControlPlaneMissionPageTest: Decodable {
    let missions: [ControlPlaneMissionTest]
    let hasMore: Bool
    let nextCursor: String?
}

private struct ControlPlaneMissionTest: Decodable {
    let mission: String
    let runCount: Int
}

private struct ControlPlaneRunPageTest: Decodable {
    let runs: [ControlPlaneRunTest]
    let hasMore: Bool
}

private struct ControlPlaneRunTest: Decodable {
    let run: String
    let mission: String?
    let state: String
    let blocker: ControlPlaneBlockerTest?
    let actionEligibility: ControlPlaneActionEligibilityTest?
}

private struct ControlPlaneBlockerTest: Decodable {
    let code: String?
    let message: String
}

private struct ControlPlaneActionEligibilityTest: Decodable {
    let run: String?
    let actions: [ControlPlaneRunActionTest]
}

private struct ControlPlaneRunActionTest: Decodable {
    let action: String
    let availability: String
    let reason: String
    let confirmation: String
}

private struct ControlPlaneEventPageTest: Decodable {
    let run: String?
    let events: [ControlPlaneEventTest]
    let hasMore: Bool
}

private struct ControlPlaneEventTest: Decodable {
    let event: String
    let sequence: Int
    let kind: String
    let task: String?
}

private struct ControlPlaneRunReviewTest: Decodable {
    let run: String?
    let resource: ControlPlaneRunTest
}

// MARK: - Capacity / runner status decode types (standard CLIResponse envelope)

private struct AgentTaskCapacityReportTest: Decodable {
    let routes: [AgentTaskCapacityRouteTest]
    let nextReset: String?
    let generatedAt: String?
}

private struct AgentTaskCapacityRouteTest: Decodable {
    let backend: String
    let scope: String
    let models: [String]
    let capacity: AgentTaskCapacityDetailTest
}

private struct AgentTaskCapacityDetailTest: Decodable {
    let state: String
    let remaining: Int?
    let accounts: [AgentTaskCapacityAccountTest]?
}

private struct AgentTaskCapacityAccountTest: Decodable {
    let account: String
    let state: String
    let remaining: Int?
    let resetAt: String?
}

private struct RunnerStatusReportTest: Decodable {
    let admissionSummary: RunnerAdmissionSummaryTest
}

private struct RunnerAdmissionSummaryTest: Decodable {
    let acceptingJobs: Bool
    let connected: Bool
    let drainingGenerationCount: Int
    let nextAction: String?
}

// MARK: - Tests

func testControlPlaneCapabilities(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: control-plane-capabilities.json")
    print("--------------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/control-plane-capabilities.json"))
    let envelope = try decoder.decode(ControlPlaneEnvelopeTest<ControlPlaneCapabilitiesTest>.self, from: data)

    guard envelope.success, envelope.data.body.ok, let capabilities = envelope.data.body.resource else {
        throw NSError(domain: "ContractTest", code: 200,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-capabilities.json did not decode ok resource"])
    }
    try assertContains(capabilities.operations.joined(separator: ","), "execute_run_action",
        message: "capabilities advertise execute_run_action")
    try assertContains(capabilities.resources.joined(separator: ","), "run",
        message: "capabilities advertise the run resource")
    print("[PASS] Capabilities envelope and operations decode")
    print("")
}

func testControlPlaneMissions(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: control-plane-missions.json")
    print("----------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/control-plane-missions.json"))
    let envelope = try decoder.decode(ControlPlaneEnvelopeTest<ControlPlaneMissionPageTest>.self, from: data)

    guard envelope.success, let page = envelope.data.body.resource, page.missions.count == 3 else {
        throw NSError(domain: "ContractTest", code: 201,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-missions.json did not decode three missions"])
    }
    guard page.hasMore, page.nextCursor != nil else {
        throw NSError(domain: "ContractTest", code: 202,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-missions.json pagination did not decode"])
    }
    guard page.missions[0].mission == "cook-detached-1b9a02ee-40d4-40df-94b9-9dd8ba2800f7", page.missions[0].runCount == 1 else {
        throw NSError(domain: "ContractTest", code: 203,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-missions.json mission fields did not decode"])
    }
    print("[PASS] Mission page and pagination cursor decode")
    print("")
}

func testControlPlaneRuns(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: control-plane-runs.json")
    print("------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/control-plane-runs.json"))
    let envelope = try decoder.decode(ControlPlaneEnvelopeTest<ControlPlaneRunPageTest>.self, from: data)

    guard envelope.success, let page = envelope.data.body.resource, page.runs.count == 3 else {
        throw NSError(domain: "ContractTest", code: 210,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-runs.json did not decode three runs"])
    }

    let first = page.runs[0]
    guard first.state == "candidate_recoverable" else {
        throw NSError(domain: "ContractTest", code: 211,
            userInfo: [NSLocalizedDescriptionKey: "run state did not decode as candidate_recoverable, got \(first.state)"])
    }
    print("[PASS] Run state decodes as candidate_recoverable")

    guard let blocker = first.blocker, blocker.code == "controller_failure" else {
        throw NSError(domain: "ContractTest", code: 212,
            userInfo: [NSLocalizedDescriptionKey: "blocker.code did not decode as controller_failure"])
    }
    print("[PASS] Blocker code decodes")

    guard let firstAction = first.actionEligibility?.actions.first, firstAction.action == "cancel" else {
        throw NSError(domain: "ContractTest", code: 213,
            userInfo: [NSLocalizedDescriptionKey: "first action_eligibility action did not decode"])
    }
    guard firstAction.availability == "unavailable" else {
        throw NSError(domain: "ContractTest", code: 214,
            userInfo: [NSLocalizedDescriptionKey: "first action availability did not decode as unavailable, got \(firstAction.availability)"])
    }
    guard firstAction.reason.contains("already terminal with state CandidateRecoverable") else {
        throw NSError(domain: "ContractTest", code: 215,
            userInfo: [NSLocalizedDescriptionKey: "first action reason did not decode Homeboy's stated reason"])
    }
    print("[PASS] First action's availability and reason decode")
    print("")
}

func testControlPlaneEvents(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: control-plane-events.json")
    print("--------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/control-plane-events.json"))
    let envelope = try decoder.decode(ControlPlaneEnvelopeTest<ControlPlaneEventPageTest>.self, from: data)

    guard envelope.success, let page = envelope.data.body.resource, page.events.count == 13 else {
        throw NSError(domain: "ContractTest", code: 220,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-events.json did not decode thirteen events"])
    }
    guard !page.hasMore else {
        throw NSError(domain: "ContractTest", code: 221,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-events.json has_more should be false"])
    }

    let sequences = page.events.map(\.sequence)
    guard sequences == sequences.sorted() else {
        throw NSError(domain: "ContractTest", code: 222,
            userInfo: [NSLocalizedDescriptionKey: "event sequences did not decode in increasing order"])
    }
    guard sequences.first == 1, sequences.last == 13 else {
        throw NSError(domain: "ContractTest", code: 223,
            userInfo: [NSLocalizedDescriptionKey: "event sequence bounds did not decode as 1...13"])
    }
    print("[PASS] Event sequence ordering decodes 1...13")

    guard page.events.last?.kind == "task.state_changed", page.events.last?.task == "cook-wp-codebox" else {
        throw NSError(domain: "ContractTest", code: 224,
            userInfo: [NSLocalizedDescriptionKey: "final event kind/task did not decode"])
    }
    print("[PASS] Event kind and task decode")
    print("")
}

func testControlPlaneReview(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: control-plane-review.json")
    print("--------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/control-plane-review.json"))
    let envelope = try decoder.decode(ControlPlaneEnvelopeTest<ControlPlaneRunReviewTest>.self, from: data)

    guard envelope.success, let review = envelope.data.body.resource else {
        throw NSError(domain: "ContractTest", code: 230,
            userInfo: [NSLocalizedDescriptionKey: "control-plane-review.json did not decode ok resource"])
    }
    guard review.resource.state == "candidate_recoverable" else {
        throw NSError(domain: "ContractTest", code: 231,
            userInfo: [NSLocalizedDescriptionKey: "review.resource.state did not decode as candidate_recoverable"])
    }
    guard review.resource.actionEligibility?.actions.contains(where: { $0.action == "promote" }) == true else {
        throw NSError(domain: "ContractTest", code: 232,
            userInfo: [NSLocalizedDescriptionKey: "review.resource.action_eligibility did not decode the promote action"])
    }
    print("[PASS] Review wraps the same run resource shape as the run list/detail")
    print("")
}

func testAgentTaskCapacity(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: agent-task-capacity.json")
    print("-------------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/agent-task-capacity.json"))
    let result = try decoder.decode(CLIResponse<AgentTaskCapacityReportTest>.self, from: data)

    guard result.success, let report = result.data else {
        throw NSError(domain: "ContractTest", code: 240,
            userInfo: [NSLocalizedDescriptionKey: "agent-task-capacity.json did not decode ok data"])
    }
    guard report.routes.count == 5 else {
        throw NSError(domain: "ContractTest", code: 241,
            userInfo: [NSLocalizedDescriptionKey: "agent-task-capacity.json did not decode five routes, got \(report.routes.count)"])
    }
    print("[PASS] Five capacity routes decode")

    guard report.nextReset == "2026-09-27T15:26:10+00:00" else {
        throw NSError(domain: "ContractTest", code: 242,
            userInfo: [NSLocalizedDescriptionKey: "next_reset did not decode"])
    }
    print("[PASS] next_reset decodes")

    let anthropicRoute = report.routes.first { $0.scope == "opencode:anthropic" }
    guard let accounts = anthropicRoute?.capacity.accounts, accounts.count == 4 else {
        throw NSError(domain: "ContractTest", code: 243,
            userInfo: [NSLocalizedDescriptionKey: "anthropic route accounts did not decode"])
    }
    let states = Set(accounts.map(\.state))
    guard states == Set(["credential_expired", "available", "exhausted"]) else {
        throw NSError(domain: "ContractTest", code: 244,
            userInfo: [NSLocalizedDescriptionKey: "account states did not decode as expected, got \(states)"])
    }
    print("[PASS] Account states decode (available/exhausted/credential_expired)")
    print("")
}

func testRunnerStatus(fixturesDir: String, decoder: JSONDecoder) throws {
    print("Test: runner-status.json")
    print("-------------------------")

    let data = try Data(contentsOf: URL(fileURLWithPath: "\(fixturesDir)/runner-status.json"))
    let result = try decoder.decode(CLIResponse<RunnerStatusReportTest>.self, from: data)

    guard result.success, let report = result.data else {
        throw NSError(domain: "ContractTest", code: 250,
            userInfo: [NSLocalizedDescriptionKey: "runner-status.json did not decode ok data"])
    }
    guard report.admissionSummary.acceptingJobs == false else {
        throw NSError(domain: "ContractTest", code: 251,
            userInfo: [NSLocalizedDescriptionKey: "admission_summary.accepting_jobs did not decode as false"])
    }
    guard report.admissionSummary.drainingGenerationCount == 23 else {
        throw NSError(domain: "ContractTest", code: 252,
            userInfo: [NSLocalizedDescriptionKey: "admission_summary.draining_generation_count did not decode"])
    }
    guard let nextAction = report.admissionSummary.nextAction, nextAction.contains("homeboy runner refresh-homeboy") else {
        throw NSError(domain: "ContractTest", code: 253,
            userInfo: [NSLocalizedDescriptionKey: "admission_summary.next_action did not decode"])
    }
    print("[PASS] Runner admission_summary decodes accepting_jobs, draining count, and next_action")
    print("")
}

// MARK: - Formatting helpers (mirror Core/API/ControlPlaneModels.swift and HomeboyCLI+MissionControlCommands.swift)

private func scopeSuffixTest(_ scope: String) -> String {
    guard let colonIndex = scope.lastIndex(of: ":") else { return scope }
    return String(scope[scope.index(after: colonIndex)...])
}

private func parseControlPlaneDateTest(_ value: String) -> Date? {
    let withFractional = ISO8601DateFormatter()
    withFractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let date = withFractional.date(from: value) {
        return date
    }

    let withoutFractional = ISO8601DateFormatter()
    withoutFractional.formatOptions = [.withInternetDateTime]
    if let date = withoutFractional.date(from: value) {
        return date
    }

    if let dotIndex = value.firstIndex(of: "."),
       let offsetStart = value[dotIndex...].firstIndex(where: { $0 == "+" || $0 == "-" || $0 == "Z" }) {
        let fraction = value[value.index(after: dotIndex)..<offsetStart].prefix(3)
        let trimmed = value[..<dotIndex] + "." + fraction + value[offsetStart...]
        if let date = withFractional.date(from: String(trimmed)) {
            return date
        }
    }

    return nil
}

func testFormattingHelpers() throws {
    print("Test: scope-label and relative-reset formatting helpers")
    print("---------------------------------------------------------")

    guard scopeSuffixTest("opencode:anthropic") == "anthropic" else {
        throw NSError(domain: "ContractTest", code: 260,
            userInfo: [NSLocalizedDescriptionKey: "scope suffix did not strip the opencode: prefix"])
    }
    guard scopeSuffixTest("opencode:zai-coding-plan") == "zai-coding-plan" else {
        throw NSError(domain: "ContractTest", code: 261,
            userInfo: [NSLocalizedDescriptionKey: "scope suffix did not decode a hyphenated account id"])
    }
    guard scopeSuffixTest("no-colon-scope") == "no-colon-scope" else {
        throw NSError(domain: "ContractTest", code: 262,
            userInfo: [NSLocalizedDescriptionKey: "scope suffix must fall back to the whole scope without a colon"])
    }
    print("[PASS] Scope suffix strips the backend prefix (opencode:anthropic -> anthropic)")

    // Homeboy core emits variable-precision fractional seconds (commonly six
    // digits), which ISO8601DateFormatter's fast path rejects. The relative
    // reset text shown per account depends on this fallback succeeding.
    guard let sixDigitFraction = parseControlPlaneDateTest("2026-09-25T02:02:58.502406+00:00") else {
        throw NSError(domain: "ContractTest", code: 263,
            userInfo: [NSLocalizedDescriptionKey: "date parsing did not fall back for six-digit fractional seconds"])
    }
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    let components = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: sixDigitFraction)
    guard components.year == 2026, components.month == 9, components.day == 25,
          components.hour == 2, components.minute == 2, components.second == 58 else {
        throw NSError(domain: "ContractTest", code: 264,
            userInfo: [NSLocalizedDescriptionKey: "six-digit fractional timestamp parsed to the wrong instant"])
    }
    print("[PASS] Six-digit fractional-second timestamps parse (fixture-observed precision)")

    let threeHoursFromNow = Date().addingTimeInterval(3 * 3600)
    let formatter = RelativeDateTimeFormatter()
    formatter.unitsStyle = .abbreviated
    let relative = formatter.localizedString(for: threeHoursFromNow, relativeTo: Date())
    guard !relative.isEmpty else {
        throw NSError(domain: "ContractTest", code: 265,
            userInfo: [NSLocalizedDescriptionKey: "relative reset formatting produced an empty string"])
    }
    print("[PASS] Relative reset formatting produces a non-empty string for a future reset")
    print("")
}
