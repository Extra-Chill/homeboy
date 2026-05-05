import Foundation

struct RunsListFilter {
    var kind: String?
    var componentId: String?
    var rigId: String?
    var status: String?
    var limit: Int?
}

struct RunsCompareFilter {
    var kind: String = "bench"
    var componentId: String?
    var rigId: String?
    var scenarioId: String?
    var status: String?
    var metrics: [String] = ["total_elapsed_ms"]
    var limit: Int?
}

@MainActor
extension HomeboyCLI {
    func runsList(filter: RunsListFilter = RunsListFilter()) async throws -> [RunSummary] {
        var args = ["runs", "list"]
        appendRunsListFilter(filter, to: &args)
        let output: RunsListOutput = try await cli.executeCommand(
            args,
            dataType: RunsListOutput.self,
            source: "Runs List",
            timeout: 30
        )
        return output.runs
    }

    func runsShow(id: String) async throws -> RunDetail {
        let output: RunsShowOutput = try await cli.executeCommand(
            ["runs", "show", id],
            dataType: RunsShowOutput.self,
            source: "Runs Show",
            timeout: 30
        )
        return output.run
    }

    func runsArtifacts(id: String) async throws -> [RunArtifact] {
        let output: RunsArtifactsOutput = try await cli.executeCommand(
            ["runs", "artifacts", id],
            dataType: RunsArtifactsOutput.self,
            source: "Runs Artifacts",
            timeout: 30
        )
        return output.artifacts
    }

    func runsFindings(
        id: String,
        tool: String? = nil,
        file: String? = nil,
        fingerprint: String? = nil,
        limit: Int? = nil
    ) async throws -> [RunFinding] {
        var args = ["runs", "findings", id]
        if let tool, !tool.isEmpty {
            args.append(contentsOf: ["--tool", tool])
        }
        if let file, !file.isEmpty {
            args.append(contentsOf: ["--file", file])
        }
        if let fingerprint, !fingerprint.isEmpty {
            args.append(contentsOf: ["--fingerprint", fingerprint])
        }
        if let limit {
            args.append(contentsOf: ["--limit", String(limit)])
        }

        let output: RunsFindingsOutput = try await cli.executeCommand(
            args,
            dataType: RunsFindingsOutput.self,
            source: "Runs Findings",
            timeout: 30
        )
        return output.findings
    }

    func runsCompare(filter: RunsCompareFilter = RunsCompareFilter()) async throws -> RunsCompareOutput {
        var args = ["runs", "compare", "--format", "json", "--kind", filter.kind]
        if let componentId = filter.componentId, !componentId.isEmpty {
            args.append(contentsOf: ["--component", componentId])
        }
        if let rigId = filter.rigId, !rigId.isEmpty {
            args.append(contentsOf: ["--rig", rigId])
        }
        if let scenarioId = filter.scenarioId, !scenarioId.isEmpty {
            args.append(contentsOf: ["--scenario", scenarioId])
        }
        if let status = filter.status, !status.isEmpty {
            args.append(contentsOf: ["--status", status])
        }
        for metric in filter.metrics where !metric.isEmpty {
            args.append(contentsOf: ["--metric", metric])
        }
        if let limit = filter.limit {
            args.append(contentsOf: ["--limit", String(limit)])
        }

        return try await cli.executeCommand(
            args,
            dataType: RunsCompareOutput.self,
            source: "Runs Compare",
            timeout: 30
        )
    }

    private func appendRunsListFilter(_ filter: RunsListFilter, to args: inout [String]) {
        if let kind = filter.kind, !kind.isEmpty {
            args.append(contentsOf: ["--kind", kind])
        }
        if let componentId = filter.componentId, !componentId.isEmpty {
            args.append(contentsOf: ["--component", componentId])
        }
        if let rigId = filter.rigId, !rigId.isEmpty {
            args.append(contentsOf: ["--rig", rigId])
        }
        if let status = filter.status, !status.isEmpty {
            args.append(contentsOf: ["--status", status])
        }
        if let limit = filter.limit {
            args.append(contentsOf: ["--limit", String(limit)])
        }
    }
}
