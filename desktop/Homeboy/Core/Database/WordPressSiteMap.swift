import Foundation

/// Config-driven WordPress site mapping for database table categorization.
/// Supports both single-site and multisite WordPress installations.
struct WordPressSiteMap {
    
    // MARK: - Core WordPress Network Table Suffixes
    
    /// WordPress multisite core network table suffixes (prefix-agnostic)
    private static let coreNetworkTableSuffixes: Set<String> = [
        "blogs", "blogmeta", "site", "sitemeta",
        "users", "usermeta", "registration_log", "signups"
    ]
    
    // MARK: - Config-Driven Site Generation
    
    /// Build sites array from config (or single "All Tables" site if not multisite)
    static func getSites(from config: MultisiteConfig?) -> [WordPressSite] {
        let prefix = config?.tablePrefix ?? "wp_"
        
        guard let config = config, config.enabled, !config.blogs.isEmpty else {
            // Single-site mode: one catch-all site
            return [WordPressSite(
                blogId: 1,
                name: "All Tables",
                domain: "",
                tablePrefix: prefix
            )]
        }
        
        // Multisite mode: map configured blogs
        return config.blogs.map { blog in
            WordPressSite(
                blogId: blog.blogId,
                name: blog.name,
                domain: blog.domain,
                tablePrefix: blog.tablePrefix(basePrefix: prefix)
            )
        }
    }
    
    /// Build network table names from config
    static func getNetworkTableNames(from config: MultisiteConfig?) -> Set<String> {
        guard let config = config, config.enabled else {
            return []  // Single-site has no network tables category
        }
        
        var tables = Set<String>()
        
        // Add core WP multisite tables with configured prefix
        for suffix in coreNetworkTableSuffixes {
            tables.insert("\(config.tablePrefix)\(suffix)")
        }
        
        // Add user-defined custom network tables (stored as full names with prefix)
        for table in config.networkTables {
            tables.insert(table)
        }
        
        return tables
    }
    
    // MARK: - Table Protection System
    
    /// Core WordPress table suffixes that should never be dropped (prefix-agnostic)
    static let protectedTableSuffixes: Set<String> = [
        // WordPress core tables (per-site)
        "commentmeta", "comments", "links", "options", "postmeta", "posts",
        "termmeta", "terms", "term_relationships", "term_taxonomy",
        // WordPress multisite core tables (network-level)
        "blogs", "blogmeta", "site", "sitemeta", "users", "usermeta",
        "registration_log", "signups"
    ]
    
    /// Detect the table prefix from a list of tables (looks for common patterns)
    static func detectTablePrefix(from tables: [DatabaseTable]) -> String? {
        let wpCoreTables = ["options", "posts", "users"]
        for table in tables {
            for coreTable in wpCoreTables {
                if table.name.hasSuffix("_\(coreTable)") {
                    let suffix = "_\(coreTable)"
                    let prefix = String(table.name.dropLast(suffix.count))
                    return prefix + "_"
                }
            }
        }
        return nil
    }
    
    /// Check if a table is protected (should not be dropped)
    static func isProtectedTable(_ tableName: String, prefix: String?) -> Bool {
        guard let prefix = prefix else { return false }
        
        // Check main site tables (prefix + suffix)
        for suffix in protectedTableSuffixes {
            if tableName == "\(prefix)\(suffix)" {
                return true
            }
        }
        
        // Check multisite tables (prefix + blogId + suffix)
        // Pattern: wp_2_posts, wp_10_options, etc.
        let escapedPrefix = NSRegularExpression.escapedPattern(for: prefix)
        let pattern = "^\(escapedPrefix)(\\d+)_(.+)$"
        if let regex = try? NSRegularExpression(pattern: pattern),
           let match = regex.firstMatch(in: tableName, range: NSRange(tableName.startIndex..., in: tableName)),
           let suffixRange = Range(match.range(at: 2), in: tableName) {
            let suffix = String(tableName[suffixRange])
            return protectedTableSuffixes.contains(suffix)
        }
        
        return false
    }
    
    // MARK: - Table Categorization
    
    /// Categorize tables into sites, network, and other
    static func categorize(
        tables: [DatabaseTable],
        config: MultisiteConfig?
    ) -> (sites: [WordPressSite], network: [DatabaseTable], other: [DatabaseTable]) {
        let prefix = config?.tablePrefix ?? "wp_"
        var sites = getSites(from: config)
        let networkTableNames = getNetworkTableNames(from: config)
        var networkTables: [DatabaseTable] = []
        var otherTables: [DatabaseTable] = []
        
        for table in tables {
            if networkTableNames.contains(table.name) {
                networkTables.append(table)
            } else if let siteIndex = findSiteIndex(for: table.name, in: sites, basePrefix: prefix) {
                sites[siteIndex].tables.append(table)
            } else {
                otherTables.append(table)
            }
        }
        
        // Sort tables within each category alphabetically
        for i in sites.indices {
            sites[i].tables.sort { $0.name < $1.name }
        }
        networkTables.sort { $0.name < $1.name }
        otherTables.sort { $0.name < $1.name }
        
        return (sites, networkTables, otherTables)
    }
    
    /// Find which site a table belongs to based on prefix
    private static func findSiteIndex(
        for tableName: String,
        in sites: [WordPressSite],
        basePrefix: String
    ) -> Int? {
        // Single-site mode: match any table with the prefix
        if sites.count == 1 {
            return tableName.hasPrefix(basePrefix) ? 0 : nil
        }
        
        // Multisite: check non-main sites first (more specific prefixes)
        // Sort by prefix length descending to avoid wp_10_ matching wp_1_
        let sortedNonMainSites = sites.enumerated()
            .filter { $0.element.blogId != 1 }
            .sorted { $0.element.tablePrefix.count > $1.element.tablePrefix.count }
        
        for (index, site) in sortedNonMainSites {
            if tableName.hasPrefix(site.tablePrefix) {
                return index
            }
        }
        
        // Check main site last (base prefix is less specific)
        if tableName.hasPrefix(basePrefix) {
            // Ensure it's not a numbered site table we missed
            let escapedPrefix = NSRegularExpression.escapedPattern(for: basePrefix)
            let pattern = "^\(escapedPrefix)\\d+_"
            if tableName.range(of: pattern, options: .regularExpression) == nil {
                return sites.firstIndex { $0.blogId == 1 }
            }
        }
        
        return nil
    }
}
