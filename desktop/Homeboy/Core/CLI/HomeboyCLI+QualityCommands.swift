import Foundation

@MainActor
extension HomeboyCLI {
    func qualityReviewSummary(
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> QualityReviewOutput {
        var args = HomeboyCommandRegistry.arguments("review", appending: [componentId, "--summary"])
        appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
        return try await executeCommandWithOutputFile(args, dataType: QualityReviewOutput.self, source: "Quality Review", timeout: 180)
    }

    func qualityStage(
        _ stage: QualityStage,
        componentId: String,
        path: String? = nil,
        scope: QualityScope = .full,
        changedSince: String? = nil
    ) async throws -> CLIBridgeResponse {
        var args: [String]
        switch stage {
        case .lint:
            args = HomeboyCommandRegistry.arguments("review lint", appending: [componentId])
        case .test:
            args = HomeboyCommandRegistry.arguments("review test", appending: [componentId])
        }
        appendQualityScope(&args, path: path, scope: scope, changedSince: changedSince)
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

    func refactorRename(componentId: String, from: String, to: String, write: Bool = false) async throws -> RefactorResult {
        var args = HomeboyCommandRegistry.arguments(
            "refactor rename",
            appending: ["--from", from, "--to", to, "--component", componentId]
        )
        if write {
            args.append("--write")
        }
        return try await cli.executeCommand(args, dataType: RefactorResult.self, source: "Refactor Rename", timeout: 120)
    }
}
