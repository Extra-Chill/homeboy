import Foundation

/// Resolves database schema patterns by combining project type definitions with project configuration.
/// This provides a generic interface for table grouping, protection patterns, and table categorization
/// that works with any project type defined via JSON.
struct SchemaResolver {
    
    // MARK: - Protected Patterns
    
    /// Resolve protected table patterns for a project.
    /// Combines type-defined suffixes with project-specific prefix.
    static func resolveProtectedPatterns(for project: ProjectConfiguration) -> [String] {
        let typeDefinition = project.typeDefinition
        guard let database = typeDefinition.database,
              let suffixes = database.protectedSuffixes else {
            // No type-defined patterns, return user-defined only
            return project.protectedTablePatterns
        }
        
        let prefix = project.tablePrefix ?? database.defaultTablePrefix ?? ""
        var patterns: [String] = []
        
        for suffix in suffixes {
            // Main site pattern: prefix + suffix (e.g., wp_options)
            patterns.append("\(prefix)\(suffix)")
            // Multisite pattern: prefix + blogId + suffix (e.g., wp_2_options)
            patterns.append("\(prefix)*_\(suffix)")
        }
        
        // Add user-defined patterns
        patterns.append(contentsOf: project.protectedTablePatterns)
        
        return patterns
    }
    
    // MARK: - Table Groupings
    
    /// Generate default table groupings for a project.
    /// Uses multisite groupings if enabled, otherwise single-site grouping.
    static func resolveDefaultGroupings(for project: ProjectConfiguration) -> [ItemGrouping] {
        let typeDefinition = project.typeDefinition
        guard let database = typeDefinition.database else {
            return []
        }
        
        let prefix = project.tablePrefix ?? database.defaultTablePrefix ?? ""
        
        // Check if multisite is enabled
        if let multisite = project.multisite, multisite.enabled {
            return resolveMultisiteGroupings(
                database: database,
                prefix: prefix,
                multisite: multisite
            )
        }
        
        // Single site - use default grouping template
        guard let template = database.defaultGrouping else {
            return []
        }
        
        let pattern = template.patternTemplate
            .replacingOccurrences(of: "{{prefix}}", with: prefix)
        
        return [
            ItemGrouping(
                id: template.id,
                name: template.name,
                memberIds: [],
                patterns: [pattern],
                sortOrder: 0
            )
        ]
    }
    
    /// Resolve multisite-specific groupings.
    private static func resolveMultisiteGroupings(
        database: DatabaseSchemaDefinition,
        prefix: String,
        multisite: MultisiteConfig
    ) -> [ItemGrouping] {
        guard let template = database.multisiteGrouping else {
            return []
        }
        
        var groupings: [ItemGrouping] = []
        var sortOrder = 0
        
        // Network tables grouping
        if let networkTemplate = template.network,
           let networkSuffixes = database.tableSuffixes?["network"] {
            var patterns = networkSuffixes.map { "\(prefix)\($0)" }
            // Add user-defined custom network tables
            patterns.append(contentsOf: multisite.networkTables)
            
            groupings.append(ItemGrouping(
                id: networkTemplate.id,
                name: networkTemplate.name,
                memberIds: [],
                patterns: patterns,
                sortOrder: sortOrder
            ))
            sortOrder += 1
        }
        
        // Per-site groupings
        if let siteTemplate = template.site {
            for blog in multisite.blogs {
                let sitePrefix = blog.tablePrefix(basePrefix: prefix)
                let siteName = blog.name.isEmpty ? "Site \(blog.blogId)" : blog.name
                
                let id = siteTemplate.idTemplate
                    .replacingOccurrences(of: "{{blogId}}", with: String(blog.blogId))
                let name = siteTemplate.nameTemplate
                    .replacingOccurrences(of: "{{siteName}}", with: siteName)
                let pattern = siteTemplate.patternTemplate
                    .replacingOccurrences(of: "{{sitePrefix}}", with: sitePrefix)
                
                groupings.append(ItemGrouping(
                    id: id,
                    name: name,
                    memberIds: [],
                    patterns: [pattern],
                    sortOrder: sortOrder
                ))
                sortOrder += 1
            }
        }
        
        return groupings
    }
    
    // MARK: - Prefix Detection
    
    /// Detect table prefix from database tables using type-defined detection suffixes.
    static func detectTablePrefix(
        from tables: [DatabaseTable],
        using typeDefinition: ProjectTypeDefinition
    ) -> String? {
        guard let database = typeDefinition.database,
              let detectionSuffixes = database.prefixDetectionSuffixes else {
            return nil
        }
        
        for table in tables {
            for suffix in detectionSuffixes {
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
    static func detectMultisiteBlogIds(
        from tables: [DatabaseTable],
        prefix: String,
        using typeDefinition: ProjectTypeDefinition
    ) -> [Int] {
        guard let database = typeDefinition.database,
              let coreSuffixes = database.tableSuffixes?["core"],
              !coreSuffixes.isEmpty else {
            return []
        }
        
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
    
    /// Represents a site in a multisite installation for table categorization.
    struct ResolvedSite: Identifiable, Equatable, Hashable {
        let blogId: Int
        let name: String
        let tablePrefix: String
        
        var id: Int { blogId }
        
        func hash(into hasher: inout Hasher) {
            hasher.combine(blogId)
        }
    }
    
    /// Build the list of sites from multisite configuration.
    static func resolveSites(for project: ProjectConfiguration) -> [ResolvedSite] {
        let typeDefinition = project.typeDefinition
        let prefix = project.tablePrefix ?? typeDefinition.database?.defaultTablePrefix ?? ""
        
        guard let multisite = project.multisite, multisite.enabled else {
            // Single site
            return [ResolvedSite(blogId: 1, name: "Main Site", tablePrefix: prefix)]
        }
        
        // Multisite - build from configured blogs
        if multisite.blogs.isEmpty {
            return [ResolvedSite(blogId: 1, name: "Main Site", tablePrefix: prefix)]
        }
        
        return multisite.blogs.map { blog in
            ResolvedSite(
                blogId: blog.blogId,
                name: blog.name.isEmpty ? "Site \(blog.blogId)" : blog.name,
                tablePrefix: blog.tablePrefix(basePrefix: prefix)
            )
        }
    }
    
    /// Build set of network table names from type definition and multisite config.
    static func resolveNetworkTables(for project: ProjectConfiguration) -> Set<String> {
        let typeDefinition = project.typeDefinition
        let prefix = project.tablePrefix ?? typeDefinition.database?.defaultTablePrefix ?? ""
        
        guard let multisite = project.multisite, multisite.enabled,
              let database = typeDefinition.database,
              let networkSuffixes = database.tableSuffixes?["network"] else {
            return []
        }
        
        var tables = Set<String>()
        
        // Add type-defined network tables
        for suffix in networkSuffixes {
            tables.insert("\(prefix)\(suffix)")
        }
        
        // Add user-defined custom network tables
        for table in multisite.networkTables {
            tables.insert(table)
        }
        
        return tables
    }
    
    /// Categorize a table into its owning site.
    /// Returns the site that owns the table, or nil if it's a network/unknown table.
    static func categorizeTables(
        _ tables: [DatabaseTable],
        for project: ProjectConfiguration
    ) -> (bySite: [ResolvedSite: [DatabaseTable]], network: [DatabaseTable], ungrouped: [DatabaseTable]) {
        let sites = resolveSites(for: project)
        let networkTables = resolveNetworkTables(for: project)
        
        var bySite: [ResolvedSite: [DatabaseTable]] = [:]
        var network: [DatabaseTable] = []
        var ungrouped: [DatabaseTable] = []
        
        // Initialize site buckets
        for site in sites {
            bySite[site] = []
        }
        
        // Sort sites by prefix length (longest first) for correct matching
        let sortedSites = sites.sorted { $0.tablePrefix.count > $1.tablePrefix.count }
        
        for table in tables {
            // Check if it's a network table
            if networkTables.contains(table.name) {
                network.append(table)
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
        
        return (bySite, network, ungrouped)
    }
}
