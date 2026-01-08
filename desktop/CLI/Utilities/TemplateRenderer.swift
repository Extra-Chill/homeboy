import Foundation

/// Renders command templates by substituting {{variable}} placeholders with values.
enum TemplateRenderer {
    
    /// Renders a template string by replacing {{variable}} placeholders with values from the provided dictionary.
    /// Unknown variables are left as-is (not replaced).
    static func render(_ template: String, variables: [String: String]) -> String {
        var result = template
        
        for (key, value) in variables {
            result = result.replacingOccurrences(of: "{{\(key)}}", with: value)
        }
        
        return result
    }
}
