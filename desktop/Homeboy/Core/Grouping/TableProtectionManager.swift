import Foundation

/// Manages table-level protection for database tables.
/// Protection is independent of grouping — a table is protected regardless of which group it belongs to.
/// Supports two tiers:
/// - Core protected tables (defined by project type schema, locked by default)
/// - User protected tables (user-added, easily removable)
/// Users can unlock core tables with explicit confirmation.
struct TableProtectionManager {
    
    /// Check if a table is protected (matches protection pattern and is not unlocked)
    static func isProtected(tableName: String, config: ProjectConfiguration) -> Bool {
        let matchesProtection = config.protectedTablePatterns.contains { pattern in
            matchesPattern(tableName, pattern: pattern)
        }
        
        guard matchesProtection else { return false }
        
        let isUnlocked = config.unlockedTablePatterns.contains { pattern in
            matchesPattern(tableName, pattern: pattern)
        }
        
        return !isUnlocked
    }
    
    /// Check if a table has been explicitly unlocked
    static func isUnlocked(tableName: String, config: ProjectConfiguration) -> Bool {
        config.unlockedTablePatterns.contains { pattern in
            matchesPattern(tableName, pattern: pattern)
        }
    }
    
    /// Check if a table is a core protected table (vs user-added)
    /// Core tables are those defined by the project type's schema
    static func isCoreProtected(tableName: String, config: ProjectConfiguration) -> Bool {
        let corePatterns = SchemaResolver.resolveProtectedPatterns(for: config)
        return corePatterns.contains { pattern in
            matchesPattern(tableName, pattern: pattern)
        }
    }
    
    /// Add a table to the protection list
    static func protect(tableName: String, in config: inout ProjectConfiguration) {
        guard !config.protectedTablePatterns.contains(tableName) else { return }
        config.protectedTablePatterns.append(tableName)
    }
    
    /// Remove a table from the protection list (only works for user-added tables)
    /// Core protected tables must be unlocked instead
    static func unprotect(tableName: String, in config: inout ProjectConfiguration) {
        config.protectedTablePatterns.removeAll { $0 == tableName }
        config.unlockedTablePatterns.removeAll { $0 == tableName }
    }
    
    /// Unlock a core protected table (allows deletion despite being in protection list)
    static func unlock(tableName: String, in config: inout ProjectConfiguration) {
        guard !config.unlockedTablePatterns.contains(tableName) else { return }
        config.unlockedTablePatterns.append(tableName)
    }
    
    /// Re-lock a previously unlocked core table
    static func lock(tableName: String, in config: inout ProjectConfiguration) {
        config.unlockedTablePatterns.removeAll { $0 == tableName }
    }
    
    /// Check if a table name matches a glob pattern
    private static func matchesPattern(_ tableName: String, pattern: String) -> Bool {
        if !pattern.contains("*") {
            return tableName == pattern
        }
        
        let regexPattern = "^" + NSRegularExpression.escapedPattern(for: pattern)
            .replacingOccurrences(of: "\\*", with: ".*") + "$"
        
        guard let regex = try? NSRegularExpression(pattern: regexPattern, options: []) else {
            return false
        }
        
        let range = NSRange(tableName.startIndex..., in: tableName)
        return regex.firstMatch(in: tableName, options: [], range: range) != nil
    }
}
