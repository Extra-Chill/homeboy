import Foundation

/// Resolves database schema patterns for WordPress projects.
/// Provides table prefix detection, blog ID detection, and table categorization by site.
struct SchemaResolver {

    // MARK: - WordPress-specific Constants

    /// Suffixes used to detect table prefix in WordPress databases
    private static let wordPressPrefixDetectionSuffixes = ["options", "posts", "users"]

    /// Core WordPress table suffixes used for multisite detection
    private static let wordPressCoreSuffixes = ["options", "posts", "postmeta", "comments", "commentmeta", "terms", "term_taxonomy", "term_relationships", "termmeta", "links"]

    /// Network/shared WordPress table suffixes (multisite)
    private static let wordPressNetworkSuffixes = ["users", "usermeta", "blogs", "blogmeta", "site", "sitemeta", "signups", "registration_log"]

    /// Default WordPress table prefix
    private static let wordPressDefaultPrefix = "wp_"

    // MARK: - Prefix Detection

    /// Detect table prefix from database tables.
    static func detectTablePrefix(from tables: [DatabaseTable]) -> String? {
        for table in tables {
            for suffix in wordPressPrefixDetectionSuffixes {
                let fullSuffix = "_\(suffix)"
                if table.name.hasSuffix(fullSuffix) {
                    let endIndex = table.name.index(table.name.endIndex, offsetBy: -suffix.count - 1)
                    return String(table.name[..<endIndex]) + "_"
                }
            }
        }
        return nil
    }

    /// Detect multisite blog IDs from database tables.
    /// Looks for tables matching pattern: prefix + number + underscore + suffix
    static func detectMultisiteBlogIds(from tables: [DatabaseTable], prefix: String) -> [Int] {
        var blogIds = Set<Int>()
        let escapedPrefix = NSRegularExpression.escapedPattern(for: prefix)
        let pattern = "^\(escapedPrefix)(\\d+)_"

        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return []
        }

        for table in tables {
            let range = NSRange(table.name.startIndex..., in: table.name)
            if let match = regex.firstMatch(in: table.name, range: range),
               let blogIdRange = Range(match.range(at: 1), in: table.name),
               let blogId = Int(table.name[blogIdRange]) {
                blogIds.insert(blogId)
            }
        }

        // Sort and always include blog 1 (main site) if any multisite tables exist
        var sortedIds = blogIds.sorted()
        if !sortedIds.isEmpty && !sortedIds.contains(1) {
            sortedIds.insert(1, at: 0)
        }

        return sortedIds
    }

    // MARK: - Table Categorization

    /// Represents a site/subtarget for table categorization.
    struct ResolvedSite: Identifiable, Equatable, Hashable {
        let number: Int
        let name: String
        let tablePrefix: String

        var id: Int { number }

        func hash(into hasher: inout Hasher) {
            hasher.combine(number)
        }
    }

    /// Build the list of sites from subtargets configuration.
    static func resolveSites(for project: ProjectConfiguration) -> [ResolvedSite] {
        let prefix = project.tablePrefix ?? wordPressDefaultPrefix

        guard project.hasSubTargets else {
            // Single site
            return [ResolvedSite(number: 1, name: "Main Site", tablePrefix: prefix)]
        }

        // Build from subtargets
        if project.subTargets.isEmpty {
            return [ResolvedSite(number: 1, name: "Main Site", tablePrefix: prefix)]
        }

        return project.subTargets.map { subTarget in
            ResolvedSite(
                number: subTarget.number ?? 1,
                name: subTarget.name.isEmpty ? "Site \(subTarget.number ?? 1)" : subTarget.name,
                tablePrefix: subTarget.tablePrefix(basePrefix: prefix)
            )
        }
    }

    /// Build set of shared/network table names for WordPress multisite.
    static func resolveSharedTables(for project: ProjectConfiguration) -> Set<String> {
        let prefix = project.tablePrefix ?? wordPressDefaultPrefix

        guard project.hasSubTargets && project.isWordPress else {
            return []
        }

        var tables = Set<String>()

        // Add WordPress network/shared tables
        for suffix in wordPressNetworkSuffixes {
            tables.insert("\(prefix)\(suffix)")
        }

        // Add user-defined custom shared tables
        for table in project.sharedTables {
            tables.insert(table)
        }

        return tables
    }

    /// Categorize a table into its owning site.
    /// Returns the site that owns the table, or nil if it's a shared/unknown table.
    static func categorizeTables(
        _ tables: [DatabaseTable],
        for project: ProjectConfiguration
    ) -> (bySite: [ResolvedSite: [DatabaseTable]], shared: [DatabaseTable], ungrouped: [DatabaseTable]) {
        let sites = resolveSites(for: project)
        let sharedTableNames = resolveSharedTables(for: project)

        var bySite: [ResolvedSite: [DatabaseTable]] = [:]
        var shared: [DatabaseTable] = []
        var ungrouped: [DatabaseTable] = []

        // Initialize site buckets
        for site in sites {
            bySite[site] = []
        }

        // Sort sites by prefix length (longest first) for correct matching
        let sortedSites = sites.sorted { $0.tablePrefix.count > $1.tablePrefix.count }

        for table in tables {
            // Check if it's a shared/network table
            if sharedTableNames.contains(table.name) {
                shared.append(table)
                continue
            }

            // Try to match to a site by prefix
            var matched = false
            for site in sortedSites {
                if table.name.hasPrefix(site.tablePrefix) {
                    bySite[site]?.append(table)
                    matched = true
                    break
                }
            }

            if !matched {
                ungrouped.append(table)
            }
        }

        return (bySite, shared, ungrouped)
    }
}
