import Foundation

@MainActor
final class AgentTasksViewModel: ObservableObject {
    @Published var runners: [HomeboyRunner] = [HomeboyRunner.localDefault]
    @Published var selectedRunnerID = HomeboyRunner.localDefault.id
    @Published var providers: JSONValue = .null
    @Published var runs: [JSONValue] = []
    @Published var selectedRunID: String?
    @Published var status: JSONValue = .null
    @Published var logs: JSONValue = .null
    @Published var artifacts: JSONValue = .null
    @Published var goal = ""
    @Published var sourceRefs = ""
    @Published var workspace = ""
    @Published var provider = ""
    @Published var concurrency = 1
    @Published var attempts = 1
    @Published var policy = "{}"
    @Published var runNow = false
    @Published var promotionWorktree = ""
    @Published var verificationCommand = ""
    @Published var promotionResult = ""
    @Published var isLoading = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared

    var selectedRunner: HomeboyRunner? {
        runners.first { $0.id == selectedRunnerID }
    }

    func load() {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Agent Tasks")
            return
        }
        Task { await refresh() }
    }

    func refresh() async {
        isLoading = true
        error = nil
        do {
            async let runnerTask = cli.runnerList()
            async let providerTask = cli.agentTaskProviders(runnerID: selectedRunnerID)
            async let runTask = cli.agentTaskList(runnerID: selectedRunnerID)
            runners = try await runnerTask
            providers = try await providerTask
            if provider.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                provider = firstAvailableProviderBackend(in: providers) ?? ""
            }
            runs = (try await runTask).value(at: ["runs"]).arrayValue ?? []
            if selectedRunID == nil || !runs.contains(where: { $0.value(at: ["run_id"]).stringValue == selectedRunID }) {
                selectedRunID = runs.first?.value(at: ["run_id"]).stringValue
            }
            await loadSelectedRun()
        } catch {
            self.error = error.toDisplayableError(source: "Agent Tasks")
        }
        isLoading = false
    }

    func loadSelectedRun() async {
        guard let selectedRunID else {
            status = .null
            logs = .null
            artifacts = .null
            return
        }
        do {
            async let statusTask = cli.agentTaskStatus(id: selectedRunID, runnerID: selectedRunnerID)
            async let logTask = cli.agentTaskLogs(id: selectedRunID, runnerID: selectedRunnerID)
            async let artifactTask = cli.agentTaskArtifacts(id: selectedRunID, runnerID: selectedRunnerID)
            status = try await statusTask
            logs = try await logTask
            artifacts = try await artifactTask
        } catch {
            self.error = error.toDisplayableError(source: "Agent Tasks")
        }
    }

    func submit() {
        guard !goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            error = AppError("A mission goal is required.", source: "Agent Tasks")
            return
        }
        guard !provider.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            error = AppError("Select an active or available provider backend before submitting.", source: "Agent Tasks")
            return
        }
        do {
            let plan = try makePlan()
            let runID = "desktop-run-\(UUID().uuidString.lowercased())"
            Task {
                do {
                    _ = try await cli.agentTaskSubmit(plan: plan, runID: runID, runNow: runNow, runnerID: selectedRunnerID)
                    selectedRunID = runID
                    await refresh()
                } catch {
                    self.error = error.toDisplayableError(source: "Agent Tasks")
                }
            }
        } catch {
            self.error = error.toDisplayableError(source: "Agent Tasks")
        }
    }

    func cancel() { perform { try await self.cli.agentTaskCancel(id: $0, runnerID: self.selectedRunnerID) } }
    func retry() { perform { try await self.cli.agentTaskRetry(id: $0, runnerID: self.selectedRunnerID) } }
    func resume() { perform { try await self.cli.agentTaskResume(id: $0, runnerID: self.selectedRunnerID) } }

    func dryRunPromotion() {
        guard let selectedRunID, !promotionWorktree.isEmpty else {
            error = AppError("Select a durable run and enter a managed worktree handle.", source: "Agent Tasks")
            return
        }
        Task {
            do {
                let result = try await cli.agentTaskPromoteDryRun(runID: selectedRunID, worktree: promotionWorktree, verify: verificationCommand, runnerID: selectedRunnerID)
                promotionResult = result.prettyPrintedJSONString
            } catch {
                self.error = error.toDisplayableError(source: "Agent Tasks")
            }
        }
    }

    private func perform(_ action: @escaping (String) async throws -> JSONValue) {
        guard let selectedRunID else { return }
        Task {
            do {
                _ = try await action(selectedRunID)
                await refresh()
            } catch {
                self.error = error.toDisplayableError(source: "Agent Tasks")
            }
        }
    }

    private func makePlan() throws -> JSONValue {
        let policyData = Data(policy.utf8)
        let policyValue = try JSONDecoder().decode(JSONValue.self, from: policyData)
        guard case .object = policyValue else {
            throw CLIBridgeError.invalidResponse("Policy must be a JSON object.")
        }

        var workspaceValue: [String: JSONValue] = [
            "mode": .string(workspace.isEmpty ? "ephemeral" : "existing")
        ]
        if !workspace.isEmpty {
            workspaceValue["root"] = .string(workspace)
        }
        let task: JSONValue = .object([
            "schema": .string("homeboy/agent-task-request/v1"),
            "task_id": .string("desktop-mission"),
            "instructions": .string(goal),
            "executor": .object(["backend": .string(provider)]),
            "source_refs": .array(sourceRefs.split(separator: "\n").map {
                .object(["kind": .string("reference"), "uri": .string(String($0))])
            }),
            "workspace": .object(workspaceValue),
            "policy": policyValue
        ])
        return .object([
            "schema": .string("homeboy/agent-task-plan/v1"),
            "plan_id": .string("desktop-mission-\(UUID().uuidString.lowercased())"),
            "tasks": .array([task]),
            "options": .object([
                "max_concurrency": .int(concurrency),
                "retry": .object(["max_attempts": .int(attempts)])
            ]),
            "metadata": .object([
                "source_refs": .array(sourceRefs.split(separator: "\n").map { .string(String($0)) })
            ])
        ])
    }

    private func firstAvailableProviderBackend(in response: JSONValue) -> String? {
        response.value(at: ["providers"]).arrayValue?
            .first {
                ["active", "available"].contains($0.value(at: ["status"]).stringValue)
                    && !$0.value(at: ["backend"]).stringValue.isEmpty
            }?
            .value(at: ["backend"]).stringValue
    }
}

extension JSONValue {
    func value(at path: [String]) -> JSONValue {
        path.reduce(self) { value, key in
            guard case .object(let object) = value else { return .null }
            return object[key] ?? .null
        }
    }

    var arrayValue: [JSONValue]? {
        guard case .array(let value) = self else { return nil }
        return value
    }
}
