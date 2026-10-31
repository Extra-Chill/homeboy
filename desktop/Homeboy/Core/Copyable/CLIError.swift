import AppKit
import Foundation

/// Hint from CLI error response providing actionable guidance
struct CLIHint: Decodable, Sendable {
    let message: String
}

/// Full-fidelity CLI error matching the CLI JSON contract.
/// Surfaces all structured error information from the CLI without transformation.
struct CLIError: CopyableContent, Sendable {
    let code: String
    let message: String
    let details: [String: JSONValue]
    let hints: [CLIHint]
    let retryable: Bool?
    let source: String

    var body: String { message }
    var contentType: ContentType { .error }

    var context: ContentContext {
        var additionalInfo: [String: String] = ["Error Code": code]

        for (key, value) in details {
            switch value {
            case .object:
                let flattened = value.flattenedKeyValues(prefix: key)
                additionalInfo.merge(flattened) { _, new in new }
            default:
                additionalInfo[key] = value.stringValue
            }
        }

        if let retryable = retryable {
            additionalInfo["Retryable"] = retryable ? "Yes" : "No"
        }

        return ContentContext.current(source: source, additionalInfo: additionalInfo)
    }

    var asMarkdown: String {
        var md = """
        ## \(title)

        ```
        \(body)
        ```

        ### Context
        | Field | Value |
        |-------|-------|
        | Timestamp | \(context.timestamp.ISO8601Format()) |
        | App | \(ContentContext.appIdentifier) |
        """

        if let project = context.projectName {
            md += "\n| Project | \(project) |"
        }

        if let server = context.serverName {
            if let host = context.serverHost {
                md += "\n| Server | \(server) (\(host)) |"
            } else {
                md += "\n| Server | \(server) |"
            }
        }

        for (key, value) in context.additionalInfo.sorted(by: { $0.key < $1.key }) {
            let formattedValue = key.lowercased().contains("path") ? "`\(value)`" : value
            md += "\n| \(key) | \(formattedValue) |"
        }

        if !hints.isEmpty {
            md += "\n\n### Hints\n"
            for hint in hints {
                md += "- \(hint.message)\n"
            }
        }

        return md
    }
}
