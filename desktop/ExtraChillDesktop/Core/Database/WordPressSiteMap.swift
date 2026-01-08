import Foundation

/// Canonical site configuration for Extra Chill multisite network.
/// Mirrors blog-ids.php from extrachill-multisite plugin.
struct WordPressSiteMap {
    
    /// All sites in the network, ordered by blog ID
    static func getSites() -> [WordPressSite] {
        return [
            WordPressSite(blogId: 1, name: "Main", domain: "extrachill.com", tablePrefix: "c8c_"),
            WordPressSite(blogId: 2, name: "Community", domain: "community.extrachill.com", tablePrefix: "c8c_2_"),
            WordPressSite(blogId: 3, name: "Shop", domain: "shop.extrachill.com", tablePrefix: "c8c_3_"),
            WordPressSite(blogId: 4, name: "Artist", domain: "artist.extrachill.com", tablePrefix: "c8c_4_"),
            WordPressSite(blogId: 5, name: "Chat", domain: "chat.extrachill.com", tablePrefix: "c8c_5_"),
            // Blog ID 6 is unused
            WordPressSite(blogId: 7, name: "Events", domain: "events.extrachill.com", tablePrefix: "c8c_7_"),
            WordPressSite(blogId: 8, name: "Stream", domain: "stream.extrachill.com", tablePrefix: "c8c_8_"),
            WordPressSite(blogId: 9, name: "Newsletter", domain: "newsletter.extrachill.com", tablePrefix: "c8c_9_"),
            WordPressSite(blogId: 10, name: "Docs", domain: "docs.extrachill.com", tablePrefix: "c8c_10_"),
            WordPressSite(blogId: 11, name: "Wire", domain: "wire.extrachill.com", tablePrefix: "c8c_11_"),
            WordPressSite(blogId: 12, name: "Horoscope", domain: "horoscope.extrachill.com", tablePrefix: "c8c_12_"),
        ]
    }
    
    /// Network-level tables shared across all sites
    static let networkTableNames: Set<String> = [
        // WordPress multisite core
        "c8c_blogs",
        "c8c_blogmeta",
        "c8c_site",
        "c8c_sitemeta",
        "c8c_users",
        "c8c_usermeta",
        "c8c_registration_log",
        "c8c_signups",
        // Extra Chill custom network tables
        "c8c_404_log",
        "c8c_extrachill_activity",
        "c8c_extrachill_analytics_events",
        "c8c_extrachill_refresh_tokens",
    ]
    
    // MARK: - Table Protection System
    
    /// Core WordPress table suffixes that should never be dropped (prefix-agnostic)
    static let protectedTableSuffixes: Set<String> = [
        // WordPress core tables (per-site)
        "commentmeta",
        "comments",
        "links",
        "options",
        "postmeta",
        "posts",
        "termmeta",
        "terms",
        "term_relationships",
        "term_taxonomy",
        
        // WordPress multisite core tables (network-level)
        "blogs",
        "blogmeta",
        "site",
        "sitemeta",
        "users",
        "usermeta",
        "registration_log",
        "signups",
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
        // Pattern: c8c_2_posts, c8c_10_options, etc.
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
    
    /// Categorize tables into sites, network, and other
    static func categorize(tables: [DatabaseTable]) -> (sites: [WordPressSite], network: [DatabaseTable], other: [DatabaseTable]) {
        var sites = getSites()
        var networkTables: [DatabaseTable] = []
        var otherTables: [DatabaseTable] = []
        
        for table in tables {
            if networkTableNames.contains(table.name) {
                networkTables.append(table)
            } else if let siteIndex = findSiteIndex(for: table.name, in: sites) {
                sites[siteIndex].tables.append(table)
            } else {
                otherTables.append(table)
            }
        }
        
        // Sort tables within each site alphabetically
        for i in sites.indices {
            sites[i].tables.sort { $0.name < $1.name }
        }
        networkTables.sort { $0.name < $1.name }
        otherTables.sort { $0.name < $1.name }
        
        return (sites, networkTables, otherTables)
    }
    
    /// Find which site a table belongs to based on prefix
    private static func findSiteIndex(for tableName: String, in sites: [WordPressSite]) -> Int? {
        // Check non-main sites first (c8c_2_, c8c_3_, etc.) - more specific prefixes
        // Must check longer prefixes first to avoid c8c_10_ matching c8c_1_
        let sortedNonMainSites = sites.enumerated()
            .filter { $0.element.blogId != 1 }
            .sorted { $0.element.tablePrefix.count > $1.element.tablePrefix.count }
        
        for (index, site) in sortedNonMainSites {
            if tableName.hasPrefix(site.tablePrefix) {
                return index
            }
        }
        
        // Check main site last (c8c_ prefix is less specific)
        // But only if it doesn't match c8c_N_ pattern for any other site
        if tableName.hasPrefix("c8c_") {
            // Make sure it's not a numbered site table we missed
            let pattern = #"^c8c_\d+_"#
            if tableName.range(of: pattern, options: .regularExpression) == nil {
                return sites.firstIndex { $0.blogId == 1 }
            }
        }
        
        return nil
    }
}
