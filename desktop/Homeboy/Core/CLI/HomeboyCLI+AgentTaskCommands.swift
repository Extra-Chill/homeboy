import Foundation

@MainActor
extension HomeboyCLI {
    func agentTaskProviders(runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["providers"], runnerID: runnerID)
    }

    func agentTaskList(runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["list", "--limit", "100"], runnerID: runnerID)
    }

    func agentTaskStatus(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["status", id, "--full"], runnerID: runnerID)
    }

    func agentTaskLogs(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["logs", id, "--full"], runnerID: runnerID)
    }

    func agentTaskArtifacts(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["artifacts", id, "--full"], runnerID: runnerID)
    }

    func agentTaskSubmit(plan: JSONValue, runID: String, runNow: Bool, runnerID: String?) async throws -> JSONValue {
        var arguments = runnerArguments(runnerID) + ["agent-task", "submit", "--plan", "-"]
        if runNow {
            // Core owns execution after durable submission; Desktop never launches a provider directly.
            arguments = runnerArguments(runnerID) + ["agent-task", "run-plan", "--plan", "-", "--record-run-id", runID]
        } else {
            arguments += ["--run-id", runID]
        }
        return try await agentTaskStdinCommand(arguments, input: plan.prettyPrintedJSONString)
    }

    func agentTaskCancel(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["cancel", id, "--reason", "Cancelled from Homeboy Desktop"], runnerID: runnerID)
    }

    func agentTaskRetry(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["retry", id, "--run"], runnerID: runnerID)
    }

    func agentTaskResume(id: String, runnerID: String?) async throws -> JSONValue {
        try await agentTaskCommand(["resume", id, "--full"], runnerID: runnerID)
    }

    func agentTaskPromoteDryRun(runID: String, worktree: String, verify: String, runnerID: String?) async throws -> JSONValue {
        var arguments = ["promote", runID, "--to-worktree", worktree, "--dry-run"]
        if !verify.isEmpty {
            arguments += ["--verify", verify]
        }
        return try await agentTaskCommand(arguments, runnerID: runnerID)
    }

    private func agentTaskCommand(_ command: [String], runnerID: String?) async throws -> JSONValue {
        try await executeCommandWithOutputFile(
            runnerArguments(runnerID) + ["agent-task"] + command,
            dataType: JSONValue.self,
            source: "Agent Tasks",
            timeout: 120
        )
    }

    private func agentTaskStdinCommand(_ arguments: [String], input: String) async throws -> JSONValue {
        let response = try await cli.executeWithStdin(arguments, stdin: input, timeout: 120)
        let result = try response.decodeResponse(JSONValue.self)
        guard result.success, let data = result.data else {
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }
        return data
    }

    private func runnerArguments(_ runnerID: String?) -> [String] {
        guard let runnerID, runnerID != HomeboyRunner.localDefault.id else { return [] }
        return ["--runner", runnerID]
    }
}
