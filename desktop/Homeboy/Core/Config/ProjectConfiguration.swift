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

// MARK: - Project Features

/// Feature flags controlling which tools are available for a project
struct ProjectFeatures: Codable, Equatable {
    var hasDatabase: Bool
    var hasRemoteDeployment: Bool
    var hasRemoteLogs: Bool
    var hasLocalCLI: Bool
    
    /// Returns sensible defaults based on project type definition
    static func defaults(for typeId: String) -> ProjectFeatures {
        let typeDefinition = ProjectTypeManager.shared.resolve(typeId)
        return ProjectFeatures(
            hasDatabase: typeDefinition.hasDatabaseBrowser,
            hasRemoteDeployment: typeDefinition.hasDeployer,
            hasRemoteLogs: typeDefinition.hasDebugLogs,
            hasLocalCLI: false  // Local CLI deferred to future phase
        )
    }
}

// MARK: - Project Configuration

/// Configuration for a single project (WordPress site, Node.js app, etc.)
struct ProjectConfiguration: Codable, Identifiable {
    var id: String
    var name: String
    var domain: String
    var projectType: String
    var features: ProjectFeatures
    
    var serverId: String?
    var basePath: String?
    var tablePrefix: String?
    
    var remoteFiles: RemoteFileConfig
    var remoteLogs: RemoteLogConfig
    var database: DatabaseConfig
    var wordpress: WordPressConfig?
    var localDev: LocalDevConfig
    var tools: ToolsConfig
    var api: APIConfig
    var multisite: MultisiteConfig?
    var components: [ComponentConfig]
    var tableGroupings: [ItemGrouping]
    var protectedTablePatterns: [String]
    var unlockedTablePatterns: [String]
    
    /// Resolved project type definition from ProjectTypeManager
    var typeDefinition: ProjectTypeDefinition {
        ProjectTypeManager.shared.resolve(projectType)
    }
    
    /// Whether this is a WordPress project
    var isWordPress: Bool {
        projectType == "wordpress"
    }
    
    /// Generic sub-targets for CLI targeting (e.g., multisite blogs, environments).
    /// Derived from MultisiteConfig for WordPress projects.
    var subTargets: [SubTarget] {
        guard let multisite = multisite, multisite.enabled else {
            return []
        }
        return multisite.blogs.map { blog in
            SubTarget(
                id: blog.name.lowercased(),
                name: blog.name,
                domain: blog.domain
            )
        }
    }
    
    /// Memberwise initializer
    init(
        id: String,
        name: String,
        domain: String,
        projectType: String,
        features: ProjectFeatures,
        serverId: String? = nil,
        basePath: String? = nil,
        tablePrefix: String? = nil,
        remoteFiles: RemoteFileConfig,
        remoteLogs: RemoteLogConfig,
        database: DatabaseConfig,
        wordpress: WordPressConfig? = nil,
        localDev: LocalDevConfig,
        tools: ToolsConfig,
        api: APIConfig,
        multisite: MultisiteConfig? = nil,
        components: [ComponentConfig],
        tableGroupings: [ItemGrouping] = [],
        protectedTablePatterns: [String] = [],
        unlockedTablePatterns: [String] = []
    ) {
        self.id = id
        self.name = name
        self.domain = domain
        self.projectType = projectType
        self.features = features
        self.serverId = serverId
        self.basePath = basePath
        self.tablePrefix = tablePrefix
        self.remoteFiles = remoteFiles
        self.remoteLogs = remoteLogs
        self.database = database
        self.wordpress = wordpress
        self.localDev = localDev
        self.tools = tools
        self.api = api
        self.multisite = multisite
        self.components = components
        self.tableGroupings = tableGroupings
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
            features: .defaults(for: projectType),
            serverId: nil,
            basePath: nil,
            tablePrefix: typeDefinition.database?.defaultTablePrefix,
            remoteFiles: .defaults(for: projectType),
            remoteLogs: .defaults(for: projectType),
            database: DatabaseConfig(),
            wordpress: projectType == "wordpress" ? WordPressConfig() : nil,
            localDev: LocalDevConfig(),
            tools: ToolsConfig(),
            api: APIConfig(),
            multisite: nil,
            components: [],
            tableGroupings: [],
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
        
        // Migration: default features based on projectType if missing
        features = try container.decodeIfPresent(ProjectFeatures.self, forKey: .features)
            ?? .defaults(for: projectType)
        
        serverId = try container.decodeIfPresent(String.self, forKey: .serverId)
        basePath = try container.decodeIfPresent(String.self, forKey: .basePath)
        
        // Decode multisite first (needed for tablePrefix migration)
        multisite = try container.decodeIfPresent(MultisiteConfig.self, forKey: .multisite)
        
        // Migration: tablePrefix moved from multisite to project level
        if let explicitPrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix) {
            tablePrefix = explicitPrefix
        } else if let legacyPrefix = multisite?.legacyTablePrefix {
            // Migrate from old multisite.tablePrefix location
            tablePrefix = legacyPrefix
        } else {
            tablePrefix = nil
        }
        
        // Migration: default remoteFiles based on projectType if missing
        remoteFiles = try container.decodeIfPresent(RemoteFileConfig.self, forKey: .remoteFiles)
            ?? .defaults(for: projectType)
        
        // Migration: default remoteLogs based on projectType if missing
        remoteLogs = try container.decodeIfPresent(RemoteLogConfig.self, forKey: .remoteLogs)
            ?? .defaults(for: projectType)
        
        database = try container.decode(DatabaseConfig.self, forKey: .database)
        wordpress = try container.decodeIfPresent(WordPressConfig.self, forKey: .wordpress)
        localDev = try container.decode(LocalDevConfig.self, forKey: .localDev)
        tools = try container.decode(ToolsConfig.self, forKey: .tools)
        api = try container.decode(APIConfig.self, forKey: .api)
        components = try container.decode([ComponentConfig].self, forKey: .components)
        
        // Migration: default to empty groupings if missing
        tableGroupings = try container.decodeIfPresent([ItemGrouping].self, forKey: .tableGroupings) ?? []
        protectedTablePatterns = try container.decodeIfPresent([String].self, forKey: .protectedTablePatterns) ?? []
        unlockedTablePatterns = try container.decodeIfPresent([String].self, forKey: .unlockedTablePatterns) ?? []
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

// MARK: - WordPress Configuration

/// WordPress-specific configuration (only used when projectType == "wordpress")
struct WordPressConfig: Codable {
    var wpContentPath: String
    
    init(wpContentPath: String = "") {
        self.wpContentPath = wpContentPath
    }
    
    var themesPath: String {
        wpContentPath.isEmpty ? "" : "\(wpContentPath)/themes"
    }
    
    var pluginsPath: String {
        wpContentPath.isEmpty ? "" : "\(wpContentPath)/plugins"
    }
    
    var isConfigured: Bool {
        !wpContentPath.isEmpty
    }
}

// MARK: - Local Development Configuration

struct LocalDevConfig: Codable {
    var wpCliPath: String
    var domain: String
    
    init(wpCliPath: String = "", domain: String = "") {
        self.wpCliPath = wpCliPath
        self.domain = domain
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
struct SubTarget: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var domain: String
}

// MARK: - Multisite Configuration

struct MultisiteConfig: Codable {
    var enabled: Bool
    var blogs: [MultisiteBlog]
    var networkTables: [String]
    
    /// Legacy tablePrefix for migration - reads from old JSON but not written back
    /// tablePrefix is now stored at the ProjectConfiguration level
    private(set) var legacyTablePrefix: String?
    
    private enum CodingKeys: String, CodingKey {
        case enabled
        case blogs
        case networkTables
        case tablePrefix  // Read-only for migration
    }
    
    init(
        enabled: Bool = false,
        blogs: [MultisiteBlog] = [],
        networkTables: [String] = []
    ) {
        self.enabled = enabled
        self.blogs = blogs
        self.networkTables = networkTables
        self.legacyTablePrefix = nil
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decode(Bool.self, forKey: .enabled)
        blogs = try container.decodeIfPresent([MultisiteBlog].self, forKey: .blogs) ?? []
        networkTables = try container.decodeIfPresent([String].self, forKey: .networkTables) ?? []
        // Read legacy tablePrefix for migration (will not be written back)
        legacyTablePrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix)
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(enabled, forKey: .enabled)
        try container.encode(blogs, forKey: .blogs)
        try container.encode(networkTables, forKey: .networkTables)
        // Note: tablePrefix is intentionally NOT encoded - it's now at project level
    }
}

struct MultisiteBlog: Codable, Identifiable {
    var blogId: Int
    var name: String
    var domain: String
    
    var id: Int { blogId }
    
    func tablePrefix(basePrefix: String) -> String {
        blogId == 1 ? basePrefix : "\(basePrefix)\(blogId)_"
    }
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
    
    // UI grouping
    var group: String?                  // e.g., "Themes", "Network Plugins", "Site Plugins"
    
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
        group: String? = nil,
        isNetwork: Bool? = nil
    ) {
        self.id = id
        self.name = name
        self.localPath = localPath
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.versionFile = versionFile
        self.versionPattern = versionPattern
        self.group = group
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
