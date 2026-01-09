import Foundation

/// Renders command templates by substituting {{variable}} placeholders with values.
enum TemplateRenderer {
    
    /// Standard template variable names
    enum Variables {
        static let projectId = "projectId"
        static let args = "args"
        static let domain = "domain"
        static let sitePath = "sitePath"
        static let cliPath = "cliPath"
        
        // Legacy compatibility
        static let basePath = "basePath"
        static let targetDomain = "targetDomain"
        
        // Database variables
        static let dbUser = "dbUser"
        static let dbPassword = "dbPassword"
        static let dbName = "dbName"
        static let dbHost = "dbHost"
        static let table = "table"
        static let query = "query"
        static let format = "format"
    }
    
    // MARK: - Legacy Static Properties (deprecated, use Variables enum)
    
    static let projectId = Variables.projectId
    static let args = Variables.args
    static let domain = Variables.domain
    static let targetDomain = Variables.targetDomain
    static let basePath = Variables.basePath
    
    static let dbUser = Variables.dbUser
    static let dbPassword = Variables.dbPassword
    static let dbName = Variables.dbName
    static let dbHost = Variables.dbHost
    static let table = Variables.table
    static let query = Variables.query
    static let format = Variables.format
    
    // MARK: - Rendering
    
    /// Renders a template string by replacing {{variable}} placeholders with values from the provided dictionary.
    /// Unknown variables are left as-is (not replaced).
    static func render(_ template: String, variables: [String: String]) -> String {
        var result = template
        
        for (key, value) in variables {
            result = result.replacingOccurrences(of: "{{\(key)}}", with: value)
        }
        
        return result
    }
    
    // MARK: - Shell Escaping
    
    /// Escapes a string for safe use in shell single-quoted strings.
    /// Handles single quotes by ending the quoted string, adding an escaped quote, and starting a new quoted string.
    static func shellEscape(_ value: String) -> String {
        value.replacingOccurrences(of: "'", with: "'\\''")
    }
}
