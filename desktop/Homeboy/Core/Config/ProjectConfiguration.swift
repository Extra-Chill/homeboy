import Foundation

// MARK: - Remote File Editor Configuration

/// Configuration for the Remote File Editor pinned files
struct RemoteFileConfig: Codable, Equatable {
    var pinnedFiles: [PinnedRemoteFile]

    init(pinnedFiles: [PinnedRemoteFile] = []) {
        self.pinnedFiles = pinnedFiles
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
/// This struct represents the Desktop's view of a project, populated from CLI output.
struct ProjectConfiguration: Codable, Identifiable {
    var id: String
    var name: String
    var domain: String

    var serverId: String?
    var basePath: String?
    var tablePrefix: String?

    var modules: [String]
    var remoteFiles: RemoteFileConfig
    var remoteLogs: RemoteLogConfig
    var database: DatabaseConfig
    var tools: ToolsConfig
    var api: APIConfig
    var subTargets: [SubTarget]
    var sharedTables: [String]
    var componentIds: [String]

    private enum CodingKeys: String, CodingKey {
        case id, name, domain
        case serverId, basePath, tablePrefix
        case modules, remoteFiles, remoteLogs, database, tools, api
        case subTargets, sharedTables, componentIds
    }

    /// Whether this is a WordPress project (inferred from modules or table prefix)
    var isWordPress: Bool {
        modules.contains("wordpress") || tablePrefix?.hasPrefix("wp") == true
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
        serverId: String? = nil,
        basePath: String? = nil,
        tablePrefix: String? = nil,
        modules: [String] = [],
        remoteFiles: RemoteFileConfig,
        remoteLogs: RemoteLogConfig,
        database: DatabaseConfig,
        tools: ToolsConfig,
        api: APIConfig,
        subTargets: [SubTarget] = [],
        sharedTables: [String] = [],
        componentIds: [String] = []
    ) {
        self.id = id
        self.name = name
        self.domain = domain
        self.serverId = serverId
        self.basePath = basePath
        self.tablePrefix = tablePrefix
        self.modules = modules
        self.remoteFiles = remoteFiles
        self.remoteLogs = remoteLogs
        self.database = database
        self.tools = tools
        self.api = api
        self.subTargets = subTargets
        self.sharedTables = sharedTables
        self.componentIds = componentIds
    }

    /// Creates a ProjectConfiguration from CLI's project show output
    init(projectId: String, config: ProjectConfigCLI) {
        self.id = projectId
        self.name = projectId  // CLI doesn't provide name, use id
        self.domain = config.domain ?? ""
        self.serverId = config.serverId
        self.basePath = config.basePath
        self.tablePrefix = config.tablePrefix
        self.modules = []  // CLI doesn't provide modules in config

        // Convert CLI remote files (generate UUIDs since CLI doesn't provide them)
        self.remoteFiles = RemoteFileConfig(
            pinnedFiles: config.remoteFiles.pinnedFiles.map { file in
                PinnedRemoteFile(id: UUID(), path: file.path, label: nil)
            }
        )

        // Convert CLI remote logs (generate UUIDs since CLI doesn't provide them)
        self.remoteLogs = RemoteLogConfig(
            pinnedLogs: config.remoteLogs.pinnedLogs.map { log in
                PinnedRemoteLog(id: UUID(), path: log.path, label: nil, tailLines: log.tailLines)
            }
        )

        // Convert CLI database config
        self.database = DatabaseConfig(
            host: config.database.host,
            port: config.database.port,
            name: config.database.name,
            user: config.database.user,
            useSSHTunnel: config.database.useSshTunnel
        )

        // Convert CLI tools config
        self.tools = ToolsConfig(
            bandcampScraper: BandcampScraperConfig(
                defaultTag: config.tools.bandcampScraper?.defaultTag ?? ""
            ),
            newsletter: NewsletterConfig(
                sendyListId: config.tools.newsletter?.sendyListId ?? ""
            )
        )

        // Convert CLI API config
        self.api = APIConfig(
            enabled: config.api.enabled,
            baseURL: config.api.baseUrl
        )

        // Convert CLI subtargets
        self.subTargets = config.subTargets.map { target in
            SubTarget(
                id: target.name.lowercased().replacingOccurrences(of: " ", with: "-"),
                name: target.name,
                domain: target.domain,
                number: target.number,
                isDefault: target.isDefault
            )
        }

        self.sharedTables = config.sharedTables
        self.componentIds = config.componentIds
    }

    /// Creates a ProjectConfiguration from CLI's ProjectListItem (minimal data for picker)
    static func fromListItem(_ item: ProjectListItem) -> ProjectConfiguration {
        var config = ProjectConfiguration.empty(id: item.id, name: item.id)
        config.domain = item.domain ?? ""
        return config
    }

    /// Creates a default empty project configuration.
    static func empty(id: String, name: String) -> ProjectConfiguration {
        ProjectConfiguration(
            id: id,
            name: name,
            domain: "",
            serverId: nil,
            basePath: nil,
            tablePrefix: nil,
            modules: [],
            remoteFiles: RemoteFileConfig(),
            remoteLogs: RemoteLogConfig(),
            database: DatabaseConfig(),
            tools: ToolsConfig(),
            api: APIConfig(),
            subTargets: [],
            sharedTables: [],
            componentIds: []
        )
    }
    
    /// Custom decoder with defaults for optional fields
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)

        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        domain = try container.decode(String.self, forKey: .domain)

        serverId = try container.decodeIfPresent(String.self, forKey: .serverId)
        basePath = try container.decodeIfPresent(String.self, forKey: .basePath)
        tablePrefix = try container.decodeIfPresent(String.self, forKey: .tablePrefix)

        modules = try container.decodeIfPresent([String].self, forKey: .modules) ?? []
        subTargets = try container.decodeIfPresent([SubTarget].self, forKey: .subTargets) ?? []
        sharedTables = try container.decodeIfPresent([String].self, forKey: .sharedTables) ?? []

        remoteFiles = try container.decodeIfPresent(RemoteFileConfig.self, forKey: .remoteFiles)
            ?? RemoteFileConfig()
        remoteLogs = try container.decodeIfPresent(RemoteLogConfig.self, forKey: .remoteLogs)
            ?? RemoteLogConfig()

        database = try container.decodeIfPresent(DatabaseConfig.self, forKey: .database)
            ?? DatabaseConfig()
        tools = try container.decodeIfPresent(ToolsConfig.self, forKey: .tools)
            ?? ToolsConfig()
        api = try container.decodeIfPresent(APIConfig.self, forKey: .api)
            ?? APIConfig()

        componentIds = try container.decodeIfPresent([String].self, forKey: .componentIds) ?? []
    }

    /// Custom encoder
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)

        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(domain, forKey: .domain)

        try container.encodeIfPresent(serverId, forKey: .serverId)
        try container.encodeIfPresent(basePath, forKey: .basePath)
        try container.encodeIfPresent(tablePrefix, forKey: .tablePrefix)

        try container.encode(modules, forKey: .modules)
        try container.encode(remoteFiles, forKey: .remoteFiles)
        try container.encode(remoteLogs, forKey: .remoteLogs)
        try container.encode(database, forKey: .database)
        try container.encode(tools, forKey: .tools)
        try container.encode(api, forKey: .api)

        try container.encode(subTargets, forKey: .subTargets)
        try container.encode(sharedTables, forKey: .sharedTables)
        try container.encode(componentIds, forKey: .componentIds)
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

// MARK: - Component Configuration

/// Legacy component configuration embedded in project JSON.
/// Used for migration to standalone ComponentConfiguration files.
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

    // Build integration - optional
    var buildCommand: String?           // Command to run in localPath (e.g., "./build.sh")

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
        buildCommand: String? = nil,
        isNetwork: Bool? = nil
    ) {
        self.id = id
        self.name = name
        self.localPath = localPath
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.versionFile = versionFile
        self.versionPattern = versionPattern
        self.buildCommand = buildCommand
        self.isNetwork = isNetwork
    }

    /// Convert to standalone ComponentConfiguration
    func toComponentConfiguration() -> ComponentConfiguration {
        ComponentConfiguration(
            id: id,
            name: name,
            localPath: localPath,
            remotePath: remotePath,
            buildArtifact: buildArtifact,
            versionFile: versionFile,
            versionPattern: versionPattern,
            buildCommand: buildCommand,
            isNetwork: isNetwork
        )
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
