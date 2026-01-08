import Foundation

/// A copyable error with context for debugging
struct AppError: CopyableContent {
    let body: String
    let context: ContentContext
    
    var contentType: ContentType { .error }
    
    /// Convenience initializer with just error message and source
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
