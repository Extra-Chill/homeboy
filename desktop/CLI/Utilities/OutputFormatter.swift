import Foundation

/// Utility functions for formatting CLI output
enum OutputFormatter {
    
    /// Formats an array of dictionaries as a simple table
    static func formatTable(_ rows: [[String: Any]], columns: [String]? = nil) -> String {
        guard !rows.isEmpty else { return "" }
        
        // Determine columns from first row if not specified
        let cols = columns ?? Array(rows[0].keys).sorted()
        
        // Calculate column widths
        var widths: [String: Int] = [:]
        for col in cols {
            widths[col] = col.count
        }
        
        for row in rows {
            for col in cols {
                let value = String(describing: row[col] ?? "")
                widths[col] = max(widths[col] ?? 0, value.count)
            }
        }
        
        // Build header
        var output = ""
        for col in cols {
            let width = widths[col] ?? col.count
            output += col.padding(toLength: width + 2, withPad: " ", startingAt: 0)
        }
        output += "\n"
        
        // Build separator
        for col in cols {
            let width = widths[col] ?? col.count
            output += String(repeating: "-", count: width) + "  "
        }
        output += "\n"
        
        // Build rows
        for row in rows {
            for col in cols {
                let value = String(describing: row[col] ?? "")
                let width = widths[col] ?? col.count
                output += value.padding(toLength: width + 2, withPad: " ", startingAt: 0)
            }
            output += "\n"
        }
        
        return output
    }
    
    /// Formats data as JSON
    static func formatJSON(_ data: Any) -> String {
        do {
            let jsonData = try JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys])
            return String(data: jsonData, encoding: .utf8) ?? "{}"
        } catch {
            return "{\"error\": \"Failed to encode JSON\"}"
        }
    }
}
