import Foundation

/// CLI-backed reads for orchestration resources that do not yet have a
/// daemon HTTP route (`homeboy/agent-task-capacity/v1`, `homeboy runner
/// status --full`'s admission summary). Everything with a `/v1/control-plane`
/// route goes through `ControlPlaneClient` instead.
@MainActor
extension HomeboyCLI {
    func agentTaskCapacity() async throws -> AgentTaskCapacityReport {
        try await cli.executeCommand(
            ["agent-task", "capacity"],
            dataType: AgentTaskCapacityReport.self,
            source: "Capacity",
            timeout: 30
        )
    }

    /// Full admission/operator status for one runner, including
    /// `admission_summary` (`accepting_jobs`, `connected`, `daemon_compatible`,
    /// `daemon_fresh`, `draining_generation_count`, `active_job_count`,
    /// `next_action`). This is distinct from `runnerStatus(id:)`, which reads
    /// a Lab tunnel connection report rather than admission evidence.
    func runnerAdmissionStatus(id: String) async throws -> RunnerStatusReport {
        try await cli.executeCommand(
            ["runner", "status", id],
            dataType: RunnerStatusReport.self,
            source: "Runners",
            timeout: 30
        )
    }
}

// MARK: - Capacity models (`homeboy/agent-task-capacity/v1`)

struct AgentTaskCapacityReport: Decodable {
    let schema: String?
    let routes: [AgentTaskCapacityRoute]
    let nextReset: String?
    let generatedAt: String?
}

struct AgentTaskCapacityRoute: Decodable, Identifiable {
    let backend: String
    let scope: String
    let selector: String?
    let models: [String]
    let capacity: AgentTaskCapacityDetail

    var id: String { scope }

    /// `opencode:anthropic` -> `anthropic`.
    var scopeSuffix: String {
        guard let colonIndex = scope.lastIndex(of: ":") else { return scope }
        return String(scope[scope.index(after: colonIndex)...])
    }
}

struct AgentTaskCapacityDetail: Decodable {
    let state: String
    let remaining: Int?
    let limit: Int?
    let unit: String?
    let resetAt: String?
    let reason: String?
    let accounts: [AgentTaskCapacityAccount]?

    /// "N% remaining", "exhausted until <local time>", or "capacity not published".
    var headline: String {
        switch state {
        case "known":
            if let remaining, unit == "percent" {
                return "\(remaining)% remaining"
            }
            if let remaining {
                return "\(remaining) remaining"
            }
            return "capacity not published"
        case "exhausted":
            if let localTime = ControlPlaneDate.localTimeString(resetAt) {
                return "exhausted until \(localTime)"
            }
            return "exhausted"
        default:
            return "capacity not published"
        }
    }
}

struct AgentTaskCapacityAccount: Decodable, Identifiable {
    let account: String
    let state: String
    let remaining: Int?
    let resetAt: String?

    var id: String { account }

    var relativeReset: String? {
        ControlPlaneDate.relative(resetAt)
    }
}

// MARK: - Runner admission status (`homeboy runner status <id> --full`)

struct RunnerStatusReport: Decodable {
    let id: String?
    let admissionSummary: RunnerAdmissionSummary?
    let operatorSummary: RunnerOperatorSummary?
}

struct RunnerAdmissionSummary: Decodable {
    let runnerId: String?
    let acceptingJobs: Bool
    let connected: Bool
    let daemonCompatible: Bool
    let daemonFresh: Bool
    let drainingGenerationCount: Int
    let activeJobCount: Int
    let nextAction: String?
    let safeToRotate: Bool?
    let daemonBuildIdentity: String?

    /// True when the summary carries any warning condition worth flagging in
    /// the Runners list: not accepting jobs, draining generations, or an
    /// incompatible/stale daemon.
    var hasWarnings: Bool {
        !acceptingJobs || drainingGenerationCount > 0 || !daemonCompatible || !daemonFresh
    }
}

struct RunnerOperatorSummary: Decodable {
    let identity: String?
    let state: String?
    let nextAction: String?
    let risk: [String]?
}
