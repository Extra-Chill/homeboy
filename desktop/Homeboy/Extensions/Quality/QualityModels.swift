import Foundation

enum QualityScope: String, CaseIterable, Identifiable {
    case full
    case changedSince
    case changedOnly

    var id: String { rawValue }

    var label: String {
        switch self {
        case .full: return "Full"
        case .changedSince: return "Changed since"
        case .changedOnly: return "Changed only"
        }
    }
}

enum QualityStage: String, CaseIterable, Identifiable {
    case audit
    case lint
    case test
    case validate

    var id: String { rawValue }

    var label: String { rawValue.capitalized }

    var icon: String {
        switch self {
        case .audit: return "magnifyingglass"
        case .lint: return "paintbrush"
        case .test: return "checklist"
        case .validate: return "hammer"
        }
    }
}

struct QualityReviewOutput: Decodable {
    let command: String
    let summary: QualityReviewSummary
    let audit: QualityStageResult?
    let lint: QualityStageResult?
    let test: QualityStageResult?
}

struct QualityReviewSummary: Decodable {
    let component: String
    let passed: Bool
    let scope: String
    let status: String
    let totalFindings: Int
}

struct QualityStageResult: Decodable, Identifiable {
    let stage: String
    let ran: Bool
    let passed: Bool
    let exitCode: Int?
    let findingCount: Int?
    let hint: String?
    let output: JSONValue?

    var id: String { stage }

    var statusLabel: String {
        guard ran else { return "Skipped" }
        return passed ? "Passed" : "Failed"
    }
}

struct AuditOutput: Decodable {
    let command: String?
    let componentId: String?
    let passed: Bool?
    let status: String?
    let findings: [AuditFinding]?
    let summary: JSONValue?
}

struct AuditFinding: Decodable, Identifiable {
    let rule: String?
    let message: String
    let file: String?
    let line: Int?
    let severity: String?

    var id: String { [rule, file, line.map(String.init), message].compactMap { $0 }.joined(separator: ":") }
}

struct RefactorPlanOutput: Decodable {
    let command: String?
    let componentId: String?
    let plan: JSONValue?
    let findings: [AuditFinding]?
    let summary: JSONValue?
}

struct RefactorResult: Decodable {
    let command: String?
    let componentId: String?
    let success: Bool?
    let changed: [String]?
    let filesChanged: [String]?
    let summary: JSONValue?
    let result: JSONValue?
}
