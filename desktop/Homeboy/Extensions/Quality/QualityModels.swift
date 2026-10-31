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
    case lint
    case test

    var id: String { rawValue }

    var label: String { rawValue.capitalized }

    var commandPath: String { "review \(rawValue)" }

    var icon: String {
        switch self {
        case .lint: return "paintbrush"
        case .test: return "checklist"
        }
    }
}

struct QualityReviewOutput: Decodable {
    let command: String
    let summary: QualityReviewSummary
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

struct RefactorResult: Decodable {
    let command: String?
    let componentId: String?
    let success: Bool?
    let changed: [String]?
    let filesChanged: [String]?
    let summary: JSONValue?
    let result: JSONValue?
}
