import Foundation

/// A named grouping of items with pattern-based and explicit membership.
/// Used for organizing database tables, deployable components, and other item types.
/// Supports exact matches, wildcard patterns, and explicit member IDs.
struct ItemGrouping: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var memberIds: [String]
    var patterns: [String]
    var sortOrder: Int
    
    init(
        id: String = UUID().uuidString,
        name: String,
        memberIds: [String] = [],
        patterns: [String] = [],
        sortOrder: Int = 0
    ) {
        self.id = id
        self.name = name
        self.memberIds = memberIds
        self.patterns = patterns
        self.sortOrder = sortOrder
    }
    
    /// Check if an item ID belongs to this grouping.
    /// Checks explicit membership first, then pattern matching.
    func contains(_ itemId: String) -> Bool {
        if memberIds.contains(itemId) {
            return true
        }
        for pattern in patterns {
            if matchesPattern(itemId, pattern: pattern) {
                return true
            }
        }
        return false
    }
    
    /// Check if an item ID matches a glob pattern.
    /// Supports exact matches and simple wildcard patterns:
    /// - `users` matches only "users"
    /// - `wp_*` matches any string starting with "wp_"
    /// - `*_cache` matches any string ending with "_cache"
    /// - `wp_*_posts` matches "wp_2_posts", "wp_10_posts", etc.
    private func matchesPattern(_ itemId: String, pattern: String) -> Bool {
        if !pattern.contains("*") {
            return itemId == pattern
        }
        
        let regexPattern = "^" + NSRegularExpression.escapedPattern(for: pattern)
            .replacingOccurrences(of: "\\*", with: ".*") + "$"
        
        guard let regex = try? NSRegularExpression(pattern: regexPattern, options: []) else {
            return false
        }
        
        let range = NSRange(itemId.startIndex..., in: itemId)
        return regex.firstMatch(in: itemId, options: [], range: range) != nil
    }
}

// MARK: - Factory Methods

extension ItemGrouping {
    
    /// Create a grouping from explicit member IDs
    static func fromMembers(
        id: String = UUID().uuidString,
        name: String,
        memberIds: [String],
        sortOrder: Int = 0
    ) -> ItemGrouping {
        ItemGrouping(
            id: id,
            name: name,
            memberIds: memberIds,
            patterns: [],
            sortOrder: sortOrder
        )
    }
    
    /// Create a grouping from patterns (for dynamic matching)
    static func fromPatterns(
        id: String = UUID().uuidString,
        name: String,
        patterns: [String],
        sortOrder: Int = 0
    ) -> ItemGrouping {
        ItemGrouping(
            id: id,
            name: name,
            memberIds: [],
            patterns: patterns,
            sortOrder: sortOrder
        )
    }
    
    /// Create a grouping with a wildcard prefix pattern
    static func withPrefix(
        _ prefix: String,
        id: String = UUID().uuidString,
        name: String,
        sortOrder: Int = 0
    ) -> ItemGrouping {
        ItemGrouping(
            id: id,
            name: name,
            memberIds: [],
            patterns: ["\(prefix)*"],
            sortOrder: sortOrder
        )
    }
}
