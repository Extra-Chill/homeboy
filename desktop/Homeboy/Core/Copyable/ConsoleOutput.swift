import Foundation

/// Copyable console output (deployment logs, command output, etc.)
struct ConsoleOutput: CopyableContent {
    let body: String
    let context: ContentContext
    
    var contentType: ContentType { .console }
    
    /// Convenience initializer with source
    init(_ output: String, source: String, additionalInfo: [String: String] = [:]) {
        self.body = output
        self.context = ContentContext.current(source: source, additionalInfo: additionalInfo)
    }
    
    /// Full initializer with custom context
    init(_ output: String, context: ContentContext) {
        self.body = output
        self.context = context
    }
}
