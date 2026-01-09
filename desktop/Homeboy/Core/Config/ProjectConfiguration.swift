import Foundation

// MARK: - Remote File Editor Configuration

/// Configuration for the Remote File Editor pinned files
struct RemoteFileConfig: Codable, Equatable {
    var pinnedFiles: [PinnedRemoteFile]
    
    init(pinnedFiles: [PinnedRemoteFile] = []) {
        self.pinnedFiles = pinnedFiles
    }
    
    /// Returns sensible defaults based on project type definition
    static func defaults(for typeId: String) -> RemoteFileConfig {
        let typeDefinition = ProjectTypeManager.shared.resolve(typeId)
        let pinnedFiles = typeDefinition.defaultPinnedFiles.map { PinnedRemoteFile(path: $0) }
        return RemoteFileConfig(pinnedFiles: pinnedFiles)
    }
}

/// A pinned remote file in the Remote File Editor
struct PinnedRemoteFile: Codable, Identifiable, Equatable {
    var id: UUID
    var path: String       // Relative to basePath
    var label: String?
    
    init(id: UUID = UUID(), path: String, label: String? = nil) {
        self.id = id
        self.path = path
        self.label = label
    }
    
    var displayName: String {
        label ?? URL(fileURLWithPath: path).lastPathComponent
    }
}

// MARK: - Remote Log Viewer Configuration

/// Configuration for the Remote Log Viewer pinned logs
struct RemoteLogConfig: Codable, Equatable {
    var pinnedLogs: [PinnedRemoteLog]
    
    init(pinnedLogs: [PinnedRemoteLog] = []) {
        self.pinnedLogs = pinnedLogs
    }
    
    /// Returns sensible defaults based on project type definition
    static func defaults(for typeId: String) -> RemoteLogConfig {
        let typeDefinition = ProjectTypeManager.shared.resolve(typeId)
        let pinnedLogs = typeDefinition.defaultPinnedLogs.map { PinnedRemoteLog(path: $0) }
        return RemoteLogConfig(pinnedLogs: pinnedLogs)
    }
}

/// A pinned remote log in the Remote Log Viewer
struct PinnedRemoteLog: Codable, Identifiable, Equatable {
    var id: UUID
    var path: String       // Relative to basePath
    var label: String?
    var tailLines: Int
    
    init(id: UUID = UUID(), path: String, label: String? = nil, tailLines: Int = 100) {
        self.id = id
        self.path = path
        self.label = label
        self.tailLines = tailLines
    }
    
    var displayName: String {
        label ?? URL(fileURLWithPath: path).lastPathComponent
    }
}

// MARK: - Project Configuration

/// Configuration for a single project (WordPress site, Node.js app, etc.)
struct ProjectConfiguration: Codable, Identifiable {
    var id: String
    var name: String
    var domain: String
    var projectType: String
    
    var serverId: String?
    var basePath: String?
    var tablePrefix: String?
    
    var remoteFiles: RemoteFileConfig
    var remoteLogs: RemoteLogConfig
    var database: DatabaseConfig
    var localCLI: LocalCLIConfig
    var tools: ToolsConfig
    var api: APIConfig
    var subTargets: [SubTarget]
    var sharedTables: [String]
    var components: [ComponentConfig]
    var tableGroupings: [ItemGrouping]
    var componentGroupings: [ItemGrouping]
    var protectedTablePatterns: [String]
    var unlockedTablePatterns: [String]
    
    /// Custom CodingKeys that includes legacy keys for migration
    private enum CodingKeys: String, CodingKey {
        case id, name, domain, projectType
        case serverId, basePath, tablePrefix
        case remoteFiles, remoteLogs, database, localCLI, tools, api
        case localDev  // Legacy key for migration only (not encoded)
        case subTargets, sharedTables, components
        case tableGroupings, componentGroupings, protectedTablePatterns, unlockedTablePatterns
        case multisite   // Legacy key for migration only (not encoded)
        case wordpress   // Legacy key for migration only (not encoded)
    }
    
    /// Resolved project type definition from ProjectTypeManager
    var typeDefinition: ProjectTypeDefinition {
        ProjectTypeManager.shared.resolve(projectType)
    }
    
    /// Whether this is a WordPress project
    var isWordPress: Bool {
        projectType == "wordpress"
    }
    
    /// Whether this project has multiple targets configured
    var hasSubTargets: Bool {
        !subTargets.isEmpty
    }
    
    /// The default subtarget, if any. Returns the first subtarget marked as default,
    /// or the first subtarget if none are marked as default.
    var defaultSubTarget: SubTarget? {
        subTargets.first { $0.isDefault } ?? subTargets.first
    }
    
    /// Memberwise initializer
    init(
        id: String,
        name: String,
        domain: String,
        projectType: String,
        serverId: String? = nil,
        basePath: String? = nil,
        tablePrefix: String? = nil,
        remoteFiles: RemoteFileConfig,
        remoteLogs: RemoteLogConfig,
        database: DatabaseConfig,
        localCLI: LocalCLIConfig,
        tools: ToolsConfig,
        api: APIConfig,
        subTargets: [SubTarget] = [],
        sharedTables: [String] = [],
        components: [ComponentConfig],
        tableGroupings: [ItemGrouping] = [],
        componentGroupings: [ItemGrouping] = [],
        protectedTablePatterns: [String] = [],
        unlockedTablePatterns: [String] = []
    ) {
        self.id = id
        self.name = name
        self.domain = domain
        self.projectType = projectType
        self.serverId = serverId
        self.basePath = basePath
        self.tablePrefix = tablePrefix
        self.remoteFiles = remoteFiles
        self.remoteLogs = remoteLogs
        self.database = database
        self.localCLI = localCLI
        self.tools = tools
        self.api = api
        self.subTargets = subTargets
        self.sharedTables = sharedTables
        self.components = components
        self.tableGroupings = tableGroupings
        self.componentGroupings = componentGroupings
        self.protectedTablePatterns = protectedTablePatterns
        self.unlockedTablePatterns = unlockedTablePatterns
    }
    
    /// Creates a default empty project configuration.
    /// Domain is intentionally empty - users configure it via Settings after project creation.
    static func empty(id: String, name: String, projectType: String = "wordpress") -> ProjectConfiguration {
        let typeDefinition = ProjectTypeManager.shared.resolve(projectType)
        return ProjectConfiguration(
            id: id,
            name: name,
            domain: "",
            projectType: projectType,
            serverId: nil,
            basePath: nil,
            tablePrefix: typeDefinition.database?.defaultTablePrefix,
            remoteFiles: .defaults(for: projectType),
            remoteLogs: .defaults(for: projectType),
            database: DatabaseConfig(),
            localCLI: LocalCLIConfig(),
            tools: ToolsConfig(),
            api: APIConfig(),
            subTargets: [],
            sharedTables: [],
            components: [],
            tableGroupings: [],
            componentGroupings: [],
            protectedTablePatterns: [],
            unlockedTablePatterns: []
        )
    }
    
    /// Custom decoder to handle migration from configs without new fields
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        
        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        domain = try container.decode(String.self, forKey: .domain)
        projectType = try container.decode(String.self, forKey: .projectType)
        
        // Note: Legacy 'features' field is ignored (backward compatibility)
        
        serverId = try container.decodeIfPresent(String.self, forKey: .serverId)
        basePath = try container.decodeIfPresent(String.self, forKey: .basePath)
        
        // Migration: Convert legacy multisite config to subTargets and sharedTables
        if let legacyMultisite = try container.decodeIfPresent(LegacyMultisiteConfig.self, forKey: .multisite),
           legacyMultisite.enabled {
            // Convert blogs to subtargets
            subTargets = legacyMultisite.blogs.map { blog in
                SubTarget(
                    id: blog.name.lowercased().replacingOccurrences(of: " ", with: "-"),
                    name: blog.name,
                    domain: blog.domain,
                    number: blog.blogId,
                    isDefault: blog.blogId == 1
                )
            }
            sharedTables = legacyMultisite.networkTables
            
            // Migrate tablePrefix from legacy multisite location if not at project level
            if let explicitPrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix) {
                tablePrefix = explicitPrefix
            } else if let legacyPrefix = legacyMultisite.legacyTablePrefix {
                tablePrefix = legacyPrefix
            } else {
                tablePrefix = nil
            }
        } else {
            // New format: read subTargets and sharedTables directly
            subTargets = try container.decodeIfPresent([SubTarget].self, forKey: .subTargets) ?? []
            sharedTables = try container.decodeIfPresent([String].self, forKey: .sharedTables) ?? []
            tablePrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix)
        }
        
        // Migration: default remoteFiles based on projectType if missing
        remoteFiles = try container.decodeIfPresent(RemoteFileConfig.self, forKey: .remoteFiles)
            ?? .defaults(for: projectType)
        
        // Migration: default remoteLogs based on projectType if missing
        remoteLogs = try container.decodeIfPresent(RemoteLogConfig.self, forKey: .remoteLogs)
            ?? .defaults(for: projectType)
        
        database = try container.decode(DatabaseConfig.self, forKey: .database)
        
        // Migration: derive basePath from legacy wordpress.wpContentPath if basePath not set
        if basePath == nil || basePath?.isEmpty == true {
            if let legacyWordpress = try container.decodeIfPresent(LegacyWordPressConfig.self, forKey: .wordpress),
               !legacyWordpress.wpContentPath.isEmpty {
                // Strip /wp-content suffix to get the WordPress root
                let wpPath = legacyWordpress.wpContentPath
                if wpPath.hasSuffix("/wp-content") {
                    basePath = String(wpPath.dropLast("/wp-content".count))
                } else {
                    basePath = (wpPath as NSString).deletingLastPathComponent
                }
            }
        }
        
        // Migration: read from localCLI or legacy localDev key
        if let cli = try container.decodeIfPresent(LocalCLIConfig.self, forKey: .localCLI) {
            localCLI = cli
        } else if let legacyCli = try container.decodeIfPresent(LocalCLIConfig.self, forKey: .localDev) {
            localCLI = legacyCli
        } else {
            localCLI = LocalCLIConfig()
        }
        tools = try container.decode(ToolsConfig.self, forKey: .tools)
        api = try container.decode(APIConfig.self, forKey: .api)
        components = try container.decode([ComponentConfig].self, forKey: .components)
        
        // Migration: default to empty groupings if missing
        tableGroupings = try container.decodeIfPresent([ItemGrouping].self, forKey: .tableGroupings) ?? []
        componentGroupings = try container.decodeIfPresent([ItemGrouping].self, forKey: .componentGroupings) ?? []
        protectedTablePatterns = try container.decodeIfPresent([String].self, forKey: .protectedTablePatterns) ?? []
        unlockedTablePatterns = try container.decodeIfPresent([String].self, forKey: .unlockedTablePatterns) ?? []
    }
    
    /// Custom encoder that writes the new format (excludes legacy multisite key)
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        
        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(domain, forKey: .domain)
        try container.encode(projectType, forKey: .projectType)
        
        try container.encodeIfPresent(serverId, forKey: .serverId)
        try container.encodeIfPresent(basePath, forKey: .basePath)
        try container.encodeIfPresent(tablePrefix, forKey: .tablePrefix)
        
        try container.encode(remoteFiles, forKey: .remoteFiles)
        try container.encode(remoteLogs, forKey: .remoteLogs)
        try container.encode(database, forKey: .database)
        try container.encode(localCLI, forKey: .localCLI)
        try container.encode(tools, forKey: .tools)
        try container.encode(api, forKey: .api)
        
        try container.encode(subTargets, forKey: .subTargets)
        try container.encode(sharedTables, forKey: .sharedTables)
        try container.encode(components, forKey: .components)
        try container.encode(tableGroupings, forKey: .tableGroupings)
        try container.encode(componentGroupings, forKey: .componentGroupings)
        try container.encode(protectedTablePatterns, forKey: .protectedTablePatterns)
        try container.encode(unlockedTablePatterns, forKey: .unlockedTablePatterns)
        
        // Note: multisite is NOT encoded - it's migrated to subTargets/sharedTables
    }
}

// MARK: - Database Configuration

struct DatabaseConfig: Codable {
    var host: String
    var port: Int
    var name: String
    var user: String
    var useSSHTunnel: Bool
    
    init(host: String = "localhost", port: Int = 3306, name: String = "", user: String = "", useSSHTunnel: Bool = true) {
        self.host = host
        self.port = port
        self.name = name
        self.user = user
        self.useSSHTunnel = useSSHTunnel
    }
}

// MARK: - Local CLI Configuration

/// Configuration for local CLI execution (e.g., WP-CLI, PM2, Artisan).
/// Used by modules and the --local flag for CLI commands.
struct LocalCLIConfig: Codable {
    var sitePath: String      // Path to local project root
    var domain: String        // Local dev domain (e.g., testing-grounds.local)
    var cliPath: String?      // Optional: explicit path to CLI binary (uses project type default if nil)
    
    init(sitePath: String = "", domain: String = "", cliPath: String? = nil) {
        self.sitePath = sitePath
        self.domain = domain
        self.cliPath = cliPath
    }
    
    /// Whether local CLI is configured (has a site path)
    var isConfigured: Bool {
        !sitePath.isEmpty
    }
    
    private enum CodingKeys: String, CodingKey {
        case sitePath
        case domain
        case cliPath
        case wpCliPath  // Legacy key for migration
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        
        // Migration: read from sitePath or legacy wpCliPath
        if let path = try container.decodeIfPresent(String.self, forKey: .sitePath) {
            sitePath = path
        } else if let legacyPath = try container.decodeIfPresent(String.self, forKey: .wpCliPath) {
            sitePath = legacyPath
        } else {
            sitePath = ""
        }
        
        domain = try container.decodeIfPresent(String.self, forKey: .domain) ?? ""
        cliPath = try container.decodeIfPresent(String.self, forKey: .cliPath)
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sitePath, forKey: .sitePath)
        try container.encode(domain, forKey: .domain)
        try container.encodeIfPresent(cliPath, forKey: .cliPath)
        // Note: wpCliPath is NOT encoded - migrated to sitePath
    }
}

// MARK: - API Configuration

struct APIConfig: Codable {
    var enabled: Bool
    var baseURL: String
    
    init(enabled: Bool = false, baseURL: String = "") {
        self.enabled = enabled
        self.baseURL = baseURL
    }
}

// MARK: - Sub-Target Configuration

/// A generic sub-target within a project (e.g., multisite blog, environment, service).
/// Used by the CLI for targeting specific domains within a project.
/// For WordPress multisite, `number` corresponds to the blog ID used for table prefix derivation.
struct SubTarget: Codable, Identifiable, Equatable {
    var id: String           // Slug identifier (e.g., "shop")
    var name: String         // Display name (e.g., "Shop")
    var domain: String       // Target domain (e.g., "shop.extrachill.com")
    var number: Int?         // Optional numeric ID (WordPress blog_id, etc.)
    var isDefault: Bool      // Primary subtarget (uses project's main domain when true)
    
    init(
        id: String,
        name: String,
        domain: String,
        number: Int? = nil,
        isDefault: Bool = false
    ) {
        self.id = id
        self.name = name
        self.domain = domain
        self.number = number
        self.isDefault = isDefault
    }
    
    /// Derives the table prefix for this subtarget.
    /// For WordPress multisite, blog ID 1 uses the base prefix, others use `prefix{number}_`.
    func tablePrefix(basePrefix: String) -> String {
        guard let number = number, number > 1 else {
            return basePrefix
        }
        return "\(basePrefix)\(number)_"
    }
}

// MARK: - Legacy Multisite Configuration (Migration Only)

/// Legacy multisite configuration for reading old JSON configs.
/// New projects use `subTargets` and `sharedTables` directly on ProjectConfiguration.
private struct LegacyMultisiteConfig: Decodable {
    let enabled: Bool
    let blogs: [LegacyMultisiteBlog]
    let networkTables: [String]
    let legacyTablePrefix: String?
    
    private enum CodingKeys: String, CodingKey {
        case enabled
        case blogs
        case networkTables
        case tablePrefix
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decode(Bool.self, forKey: .enabled)
        blogs = try container.decodeIfPresent([LegacyMultisiteBlog].self, forKey: .blogs) ?? []
        networkTables = try container.decodeIfPresent([String].self, forKey: .networkTables) ?? []
        legacyTablePrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix)
    }
}

/// Legacy multisite blog for reading old JSON configs.
private struct LegacyMultisiteBlog: Decodable {
    let blogId: Int
    let name: String
    let domain: String
}

/// Legacy WordPress config for reading old JSON configs (migration only).
/// basePath is now derived from wpContentPath during migration.
private struct LegacyWordPressConfig: Decodable {
    let wpContentPath: String
}

// MARK: - Component Configuration

struct ComponentConfig: Codable, Identifiable {
    var id: String
    var name: String
    var localPath: String
    
    // Deployment paths - explicit
    var remotePath: String              // Relative to basePath (e.g., "plugins/my-plugin")
    var buildArtifact: String           // Relative to localPath (e.g., "build/my-plugin.zip")
    
    // Version detection - optional
    var versionFile: String?            // Relative to localPath (e.g., "my-plugin.php")
    var versionPattern: String?         // Regex with capture group
    
    // Legacy WordPress compat
    var isNetwork: Bool?
    
    init(
        id: String,
        name: String,
        localPath: String,
        remotePath: String,
        buildArtifact: String,
        versionFile: String? = nil,
        versionPattern: String? = nil,
        isNetwork: Bool? = nil
    ) {
        self.id = id
        self.name = name
        self.localPath = localPath
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.versionFile = versionFile
        self.versionPattern = versionPattern
        self.isNetwork = isNetwork
    }
}

// MARK: - Tools Configuration

struct ToolsConfig: Codable {
    var bandcampScraper: BandcampScraperConfig
    var newsletter: NewsletterConfig
    
    init(bandcampScraper: BandcampScraperConfig = BandcampScraperConfig(), newsletter: NewsletterConfig = NewsletterConfig()) {
        self.bandcampScraper = bandcampScraper
        self.newsletter = newsletter
    }
}

struct BandcampScraperConfig: Codable {
    var defaultTag: String
    
    init(defaultTag: String = "") {
        self.defaultTag = defaultTag
    }
}

struct NewsletterConfig: Codable {
    var sendyListId: String
    
    init(sendyListId: String = "") {
        self.sendyListId = sendyListId
    }
}
