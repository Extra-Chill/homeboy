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

// MARK: - Module-Specific Factory Methods

extension AppError {

    static func deployer(_ message: String, path: String? = nil) -> AppError {
        AppError(message, source: "Deployer", path: path)
    }

    static func logViewer(_ message: String, path: String? = nil) -> AppError {
        AppError(message, source: "Log Viewer", path: path)
    }

    static func fileEditor(_ message: String, path: String? = nil) -> AppError {
        AppError(message, source: "File Editor", path: path)
    }

    static func databaseBrowser(_ message: String, path: String? = nil) -> AppError {
        AppError(message, source: "Database Browser", path: path)
    }

    static func module(_ moduleId: String, _ message: String) -> AppError {
        AppError(message, source: "Module: \(moduleId)")
    }
}
