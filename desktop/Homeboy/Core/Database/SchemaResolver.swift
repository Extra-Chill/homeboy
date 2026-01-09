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
    /// Uses subtarget groupings if subtargets exist, otherwise single-site grouping.
    static func resolveDefaultGroupings(for project: ProjectConfiguration) -> [ItemGrouping] {
        let typeDefinition = project.typeDefinition
        guard let database = typeDefinition.database else {
            return []
        }
        
        let prefix = project.tablePrefix ?? database.defaultTablePrefix ?? ""
        
        // Check if project has subtargets (e.g., WordPress multisite)
        if project.hasSubTargets {
            return resolveSubTargetGroupings(
                database: database,
                prefix: prefix,
                subTargets: project.subTargets,
                sharedTables: project.sharedTables
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
    
    /// Resolve subtarget-specific groupings (e.g., WordPress multisite blogs).
    private static func resolveSubTargetGroupings(
        database: DatabaseSchemaDefinition,
        prefix: String,
        subTargets: [SubTarget],
        sharedTables: [String]
    ) -> [ItemGrouping] {
        guard let template = database.multisiteGrouping else {
            return []
        }
        
        var groupings: [ItemGrouping] = []
        var sortOrder = 0
        
        // Shared/Network tables grouping
        if let networkTemplate = template.network,
           let networkSuffixes = database.tableSuffixes?["network"] {
            var patterns = networkSuffixes.map { "\(prefix)\($0)" }
            // Add user-defined custom shared tables
            patterns.append(contentsOf: sharedTables)
            
            groupings.append(ItemGrouping(
                id: networkTemplate.id,
                name: networkTemplate.name,
                memberIds: [],
                patterns: patterns,
                sortOrder: sortOrder
            ))
            sortOrder += 1
        }
        
        // Per-subtarget groupings
        if let siteTemplate = template.site {
            for subTarget in subTargets {
                let sitePrefix = subTarget.tablePrefix(basePrefix: prefix)
                let siteName = subTarget.name.isEmpty ? "Site \(subTarget.number ?? 0)" : subTarget.name
                
                let id = siteTemplate.idTemplate
                    .replacingOccurrences(of: "{{blogId}}", with: String(subTarget.number ?? 0))
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
        let typeDefinition = project.typeDefinition
        let prefix = project.tablePrefix ?? typeDefinition.database?.defaultTablePrefix ?? ""
        
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
    
    /// Build set of shared/network table names from type definition and project config.
    static func resolveSharedTables(for project: ProjectConfiguration) -> Set<String> {
        let typeDefinition = project.typeDefinition
        let prefix = project.tablePrefix ?? typeDefinition.database?.defaultTablePrefix ?? ""
        
        guard project.hasSubTargets,
              let database = typeDefinition.database,
              let networkSuffixes = database.tableSuffixes?["network"] else {
            return []
        }
        
        var tables = Set<String>()
        
        // Add type-defined network/shared tables
        for suffix in networkSuffixes {
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
