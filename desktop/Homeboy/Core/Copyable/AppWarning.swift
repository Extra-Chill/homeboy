import Foundation

/// A copyable warning with context for debugging
struct AppWarning: CopyableContent {
    let body: String
    let context: ContentContext
    
    var contentType: ContentType { .warning }
    
    /// Convenience initializer with just warning message and source
    init(_ message: String, source: String, additionalInfo: [String: String] = [:]) {
        self.body = message
        self.context = ContentContext.current(source: source, additionalInfo: additionalInfo)
    }
    
    /// Convenience initializer with path context
    init(_ message: String, source: String, path: String?, additionalInfo: [String: String] = [:]) {
        self.body = message
        self.context = ContentContext.current(source: source, path: path, additionalInfo: additionalInfo)
    }
    
    /// Full initializer with custom context
    init(_ message: String, context: ContentContext) {
        self.body = message
        self.context = context
    }
}
