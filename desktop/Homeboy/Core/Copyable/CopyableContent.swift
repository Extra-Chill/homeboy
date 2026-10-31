import AppKit
import Foundation

/// Types of copyable content for formatting purposes
enum ContentType {
    case error
    case console
    case report
    case info
    case warning
    
    var markdownHeader: String {
        switch self {
        case .error: return "Error"
        case .console: return "Console"
        case .report: return "Report"
        case .info: return "Info"
        case .warning: return "Warning"
        }
    }
}

/// Protocol for content that can be copied as markdown
protocol CopyableContent {
    var title: String { get }
    var body: String { get }
    var context: ContentContext { get }
    var contentType: ContentType { get }
    var asMarkdown: String { get }
    
    func copyToClipboard()
}

// MARK: - Default Implementation

extension CopyableContent {
    var title: String {
        "\(contentType.markdownHeader): \(context.source)"
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
        
        // Add additional info sorted by key
        for (key, value) in context.additionalInfo.sorted(by: { $0.key < $1.key }) {
            // Wrap paths in backticks for markdown
            let formattedValue = key.lowercased().contains("path") ? "`\(value)`" : value
            md += "\n| \(key) | \(formattedValue) |"
        }
        
        return md
    }
    
    func copyToClipboard() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(asMarkdown, forType: .string)
    }
}
