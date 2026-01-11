import Foundation

/// Connection status for database browser
enum DatabaseConnectionStatus: Equatable {
    case disconnected
    case connecting
    case connected
    case error(String)
    
    var isConnected: Bool {
        if case .connected = self { return true }
        return false
    }
    
    var statusText: String {
        switch self {
        case .disconnected: return "Disconnected"
        case .connecting: return "Connecting..."
        case .connected: return "Connected"
        case .error(let message): return "Error: \(message)"
        }
    }
    
    var statusColor: String {
        switch self {
        case .disconnected: return "secondary"
        case .connecting: return "orange"
        case .connected: return "green"
        case .error: return "red"
        }
    }
}

/// Represents a database table with metadata
struct DatabaseTable: Identifiable, Hashable {
    let name: String
    let rowCount: Int
    let engine: String
    let dataLength: Int64
    
    var id: String { name }
    
    /// Formatted size string (KB, MB, etc.)
    var formattedSize: String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: dataLength)
    }
}

/// Represents a column in a database table
struct DatabaseColumn: Identifiable, Hashable {
    let name: String
    let type: String
    let isNullable: Bool
    let isPrimaryKey: Bool
    let defaultValue: String?
    
    var id: String { name }
}

/// Represents a row of data - values keyed by column name
struct TableRow: Identifiable {
    let id: Int
    let values: [String: String?]
    
    /// Gets value for a column, returning empty string for nil
    func value(for column: String) -> String {
        if let value = values[column] {
            return value ?? ""
        }
        return ""
    }
}

/// Represents a WordPress site in the multisite network
struct WordPressSite: Identifiable {
    let blogId: Int
    let name: String
    let domain: String
    let tablePrefix: String
    var tables: [DatabaseTable] = []
    var isExpanded: Bool = false
    
    var id: Int { blogId }
    
    var displayName: String {
        "\(name) (\(domain))"
    }
    
    var tableCount: Int {
        tables.count
    }
}

/// Table category for non-site tables (Network, Other)
struct TableCategory: Identifiable {
    let name: String
    var tables: [DatabaseTable] = []
    var isExpanded: Bool = false
    
    var id: String { name }
    
    var tableCount: Int {
        tables.count
    }
}

// MARK: - Deletion Types

/// Represents a pending row deletion requiring confirmation
struct PendingRowDeletion: Identifiable {
    let id = UUID()
    let table: String
    let primaryKeyColumn: String
    let primaryKeyValue: String
    let rowPreview: String
}

/// Represents a pending table deletion requiring confirmation
struct PendingTableDeletion: Identifiable {
    let id = UUID()
    let table: DatabaseTable
    let isProtected: Bool
}
