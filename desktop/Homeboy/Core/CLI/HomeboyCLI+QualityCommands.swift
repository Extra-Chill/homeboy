import Foundation

@MainActor
extension HomeboyCLI {
    // MARK: - Quality Commands

    private enum AuditSlice {
        static let code = [
            "dead_guard",
            "stale_cli_argument_shape",
            "stale_cli_invocation",
            "unreferenced_export",
            "unused_parameter",
        ]

        static let docs = [
            "broken_doc_reference",
            "stale_doc_reference",
        ]

        static let structure = [
            "directory_sprawl",
            "duplicate_function",
            "god_file",
            "high_item_count",
            "missing_interface",
            "missing_method",
            "missing_registration",
            "naming_mismatch",
            "parallel_implementation",
            "repeated_field_pattern",
            "repeated_literal_shape",
            "shared_scaffolding",
        ]
    }

    private func audit(componentId: String, only kinds: [String], source: String, timeout: TimeInterval) async throws -> AuditOutput {
        var args = ["audit", componentId]
        for kind in kinds {
            args.append(contentsOf: ["--only", kind])
        }

        let response = try await cli.execute(args, timeout: timeout)
        let result = try response.decodeResponse(AuditOutput.self)

        if let data = result.data {
            return data
        }

        if let errorDetail = result.error {
            throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
        }
        throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
    }

    /// Run code audit on a component
    func auditCode(componentId: String, fix: Bool = false, write: Bool = false) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.code, source: "Audit Code", timeout: 120)
    }

    /// Run documentation audit on a component
    func auditDocs(componentId: String, fix: Bool = false) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.docs, source: "Audit Docs", timeout: 60)
    }

    /// Run structural audit on a component
    func auditStructure(componentId: String) async throws -> AuditOutput {
        try await audit(componentId: componentId, only: AuditSlice.structure, source: "Audit Structure", timeout: 60)
    }

    func qualityReviewSummary(
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> QualityReviewOutput {
        var args = ["review", componentId, "--summary"]
        appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
        return try await executeCommandWithOutputFile(args, dataType: QualityReviewOutput.self, source: "Quality Review", timeout: 180)
    }

    func qualityTriage(componentId: String) async throws -> QualityTriageOutput {
        try await executeCommandWithOutputFile(
            ["triage", "component", componentId],
            dataType: QualityTriageOutput.self,
            source: "Quality Triage",
            timeout: 60
        )
    }

    func qualityStage(
        _ stage: QualityStage,
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> CLIBridgeResponse {
        var args = [stage.rawValue, componentId]
        switch stage {
        case .audit, .lint, .test:
            appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
        case .validate:
            if let path, !path.isEmpty {
                args.append(contentsOf: ["--path", path])
            }
        }
        return try await cli.execute(args, timeout: 180)
    }

    private func appendQualityScope(
        _ args: inout [String],
        path: String?,
        scope: QualityScope,
        changedSince: String?
    ) {
        if let path, !path.isEmpty {
            args.append(contentsOf: ["--path", path])
        }

        switch scope {
        case .full:
            break
        case .changedSince:
            if let changedSince, !changedSince.isEmpty {
                args.append(contentsOf: ["--changed-since", changedSince])
            }
        case .changedOnly:
            args.append("--changed-only")
        }
    }

    // MARK: - Refactor Commands

    /// Generate a refactoring plan for a component
    func refactorPlan(componentId: String, from: String? = nil) async throws -> RefactorPlanOutput {
        var args = ["refactor", componentId]
        if let fromSource = from {
            args.append(contentsOf: ["--from", fromSource])
        }
        return try await cli.executeCommand(args, dataType: RefactorPlanOutput.self, source: "Refactor Plan", timeout: 60)
    }

    /// Apply a refactoring to a component
    func refactorApply(componentId: String, write: Bool = true) async throws -> RefactorResult {
        try await cli.executeCommand(
            ["refactor", componentId, "--write"],
            dataType: RefactorResult.self,
            source: "Refactor Apply",
            timeout: 120
        )
    }

    /// Decompose a large source file into smaller modules
    func refactorDecompose(componentId: String, file: String) async throws -> RefactorResult {
        try await cli.executeCommand(
            ["refactor", "decompose", componentId, file],
            dataType: RefactorResult.self,
            source: "Refactor Decompose",
            timeout: 120
        )
    }

    /// Rename a term across the codebase
    func refactorRename(componentId: String, from: String, to: String, write: Bool = false) async throws -> RefactorResult {
        var args = ["refactor", "rename", "--from", from, "--to", to, "--component", componentId]
        if write {
            args.append("--write")
        }
        return try await cli.executeCommand(args, dataType: RefactorResult.self, source: "Refactor Rename", timeout: 120)
    }

    // MARK: - Command Surface

    /// Check whether the installed CLI exposes a command or option.
    func commandSurfaceSupports(command: String, option: String? = nil) async throws -> Bool {
        var args = command.split(separator: " ").map(String.init)
        args.append("--help")

        let response = try await cli.execute(args, timeout: 10)
        guard response.success else {
            return false
        }

        guard let option else {
            return true
        }

        return response.output.contains(option) || response.errorOutput.contains(option)
    }

}
