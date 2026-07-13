import Foundation

func runAgentTaskContractTests(testDir: String, fixturesDir: String) throws {
    print("Test: agent-task mission-control contract")
    print("-----------------------------------------")

    let fixtureURL = URL(fileURLWithPath: "\(fixturesDir)/agent-task-plan.json")
    let data = try Data(contentsOf: fixtureURL)
    let fixture = try JSONSerialization.jsonObject(with: data) as! [String: Any]
    let task = try requiredObject(requiredArray(fixture, "tasks")[0] as! [String: Any])
    let options = try requiredObject(fixture["options"])
    let retry = try requiredObject(options["retry"])

    try assertEqual(fixture["schema"] as? String, "homeboy/agent-task-plan/v1", "plan uses the v1 schema")
    try assertEqual(fixture["plan_id"] as? String, "desktop-mission-fixture", "plan uses plan_id, not id")
    try assertEqual(task["schema"] as? String, "homeboy/agent-task-request/v1", "task uses the request schema")
    try assertEqual(task["task_id"] as? String, "desktop-mission", "task uses task_id, not id")
    try assertEqual((try requiredObject(task["executor"]))["backend"] as? String, "opencode", "task declares executor.backend")
    try assertTrue(task["workspace"] is [String: Any], "task workspace is structured")
    try assertTrue(task["policy"] is [String: Any], "task policy is structured")
    try assertEqual(options["max_concurrency"] as? Int, 2, "concurrency belongs under options")
    try assertEqual(retry["max_attempts"] as? Int, 3, "retry count belongs under options.retry")
    try assertTrue(fixture["max_attempts"] == nil, "plan has no invented top-level max_attempts")

    let repoRoot = URL(fileURLWithPath: testDir).deletingLastPathComponent()
    let viewModel = try String(contentsOf: repoRoot.appendingPathComponent("Homeboy/Views/AgentTasks/AgentTasksViewModel.swift"), encoding: .utf8)
    let commands = try String(contentsOf: repoRoot.appendingPathComponent("Homeboy/Core/CLI/HomeboyCLI+AgentTaskCommands.swift"), encoding: .utf8)
    try assertContains(viewModel, "\"plan_id\"", message: "mission encoder uses plan_id")
    try assertContains(viewModel, "\"task_id\"", message: "mission encoder uses task_id")
    try assertContains(viewModel, "\"options\"", message: "mission encoder emits options")
    try assertContains(viewModel, "\"retry\": .object([\"max_attempts\"", message: "mission encoder emits options.retry.max_attempts")
    try assertDoesNotContain(viewModel, "\n            \"max_attempts\": .int(attempts),", message: "mission encoder omits top-level max_attempts")
    try assertContains(viewModel, "value(at: [\"runs\"])", message: "run list reads the data payload directly")
    try assertDoesNotContain(viewModel, "aggregate_path", message: "promotion does not expose runner-local aggregate paths")
    try assertContains(viewModel, "firstAvailableProviderBackend", message: "provider backend defaults from the provider contract")
    try assertContains(viewModel, "[\"active\", \"available\"]", message: "provider default accepts active or available backends")
    try assertContains(viewModel, "Select an active or available provider backend", message: "submit blocks without a provider backend")
    try assertContains(commands, "[\"agent-task\", \"run-plan\", \"--plan\", \"-\", \"--record-run-id\", runID]", message: "run-now uses the exact durable run-plan argv")
    try assertContains(commands, "[\"promote\", runID, \"--to-worktree\", worktree, \"--dry-run\"]", message: "promotion uses the durable run id")
    print("[PASS] Agent-task plan and CLI argv contract")
    print("")
}

private func requiredObject(_ value: Any?) throws -> [String: Any] {
    guard let object = value as? [String: Any] else {
        throw NSError(domain: "AgentTaskContract", code: 1, userInfo: [NSLocalizedDescriptionKey: "Expected JSON object"])
    }
    return object
}

private func requiredArray(_ object: [String: Any], _ key: String) throws -> [Any] {
    guard let array = object[key] as? [Any], !array.isEmpty else {
        throw NSError(domain: "AgentTaskContract", code: 2, userInfo: [NSLocalizedDescriptionKey: "Expected non-empty \(key) array"])
    }
    return array
}

private func assertEqual<T: Equatable>(_ actual: T?, _ expected: T, _ message: String) throws {
    guard actual == expected else {
        throw NSError(domain: "AgentTaskContract", code: 3, userInfo: [NSLocalizedDescriptionKey: message])
    }
    print("[PASS] \(message)")
}

private func assertTrue(_ condition: Bool, _ message: String) throws {
    guard condition else {
        throw NSError(domain: "AgentTaskContract", code: 4, userInfo: [NSLocalizedDescriptionKey: message])
    }
    print("[PASS] \(message)")
}
