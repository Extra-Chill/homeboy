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

/// Component attachment stored on a Homeboy project.
struct ProjectComponentAttachment: Codable, Equatable {
    var id: String
    var localPath: String

    init(id: String, localPath: String = "") {
        self.id = id
        self.localPath = localPath
    }
}

/// Project-scoped overrides for a linked component.
struct ProjectComponentOverrides: Codable, Equatable {
    var remotePath: String?
    var buildArtifact: String?
    var extractCommand: String?
    var remoteOwner: String?
    var deployStrategy: String?
    var gitDeploy: GitDeployConfig?
    var hooks: [String: [String]]
    var scopes: ScopeConfig?
    var cliPath: String?

    init(
        remotePath: String? = nil,
        buildArtifact: String? = nil,
        extractCommand: String? = nil,
        remoteOwner: String? = nil,
        deployStrategy: String? = nil,
        gitDeploy: GitDeployConfig? = nil,
        hooks: [String: [String]] = [:],
        scopes: ScopeConfig? = nil,
        cliPath: String? = nil
    ) {
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.extractCommand = extractCommand
        self.remoteOwner = remoteOwner
        self.deployStrategy = deployStrategy
        self.gitDeploy = gitDeploy
        self.hooks = hooks
        self.scopes = scopes
        self.cliPath = cliPath
    }

    private enum CodingKeys: String, CodingKey {
        case remotePath, buildArtifact, extractCommand, remoteOwner, deployStrategy
        case gitDeploy, hooks, scopes, cliPath
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        remotePath = try container.decodeIfPresent(String.self, forKey: .remotePath)
        buildArtifact = try container.decodeIfPresent(String.self, forKey: .buildArtifact)
        extractCommand = try container.decodeIfPresent(String.self, forKey: .extractCommand)
        remoteOwner = try container.decodeIfPresent(String.self, forKey: .remoteOwner)
        deployStrategy = try container.decodeIfPresent(String.self, forKey: .deployStrategy)
        gitDeploy = try container.decodeIfPresent(GitDeployConfig.self, forKey: .gitDeploy)
        hooks = try container.decodeIfPresent([String: [String]].self, forKey: .hooks) ?? [:]
        scopes = try container.decodeIfPresent(ScopeConfig.self, forKey: .scopes)
        cliPath = try container.decodeIfPresent(String.self, forKey: .cliPath)
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

    var extensions: [String]
    var remoteFiles: RemoteFileConfig
    var tools: ToolsConfig
    var subTargets: [SubTarget]
    var sharedTables: [String]
    var components: [ProjectComponentAttachment]
    var componentIds: [String]
    var componentOverrides: [String: ProjectComponentOverrides]
    var services: [String]
    var changelogNextSectionLabel: String?
    var changelogNextSectionAliases: [String]?

    private enum CodingKeys: String, CodingKey {
        case id, name, domain
        case serverId, basePath, tablePrefix
        case extensions, remoteFiles, tools
        case subTargets, sharedTables, components, componentIds, componentOverrides, services
        case changelogNextSectionLabel, changelogNextSectionAliases
    }

    /// Whether this is a WordPress project (inferred from extensions or table prefix)
    var isWordPress: Bool {
        extensions.contains("wordpress") || tablePrefix?.hasPrefix("wp") == true
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
        extensions: [String] = [],
        remoteFiles: RemoteFileConfig,
        tools: ToolsConfig,
        subTargets: [SubTarget] = [],
        sharedTables: [String] = [],
        components: [ProjectComponentAttachment] = [],
        componentIds: [String] = [],
        componentOverrides: [String: ProjectComponentOverrides] = [:],
        services: [String] = [],
        changelogNextSectionLabel: String? = nil,
        changelogNextSectionAliases: [String]? = nil
    ) {
        self.id = id
        self.name = name
        self.domain = domain
        self.serverId = serverId
        self.basePath = basePath
        self.tablePrefix = tablePrefix
        self.extensions = extensions
        self.remoteFiles = remoteFiles
        self.tools = tools
        self.subTargets = subTargets
        self.sharedTables = sharedTables
        self.components = components
        self.componentIds = componentIds
        self.componentOverrides = componentOverrides
        self.services = services
        self.changelogNextSectionLabel = changelogNextSectionLabel
        self.changelogNextSectionAliases = changelogNextSectionAliases
    }

    /// Creates a ProjectConfiguration from CLI's project show output
    init(projectId: String, config: ProjectConfigCLI) {
        self.id = projectId
        self.name = projectId  // CLI doesn't provide name, use id
        self.domain = config.domain ?? ""
        self.serverId = config.serverId
        self.basePath = config.basePath
        self.tablePrefix = config.tablePrefix
        self.extensions = config.extensions.keys.sorted()

        // Convert CLI remote files (generate UUIDs since CLI doesn't provide them)
        self.remoteFiles = RemoteFileConfig(
            pinnedFiles: config.remoteFiles.pinnedFiles.map { file in
                PinnedRemoteFile(id: UUID(), path: file.path, label: nil)
            }
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
        self.components = config.components.map { component in
            ProjectComponentAttachment(id: component.id, localPath: component.localPath)
        }
        self.componentIds = config.componentIds
        self.componentOverrides = config.componentOverrides
        self.services = config.services
        self.changelogNextSectionLabel = config.changelogNextSectionLabel
        self.changelogNextSectionAliases = config.changelogNextSectionAliases
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
            extensions: [],
            remoteFiles: RemoteFileConfig(),
            tools: ToolsConfig(),
            subTargets: [],
            sharedTables: [],
            components: [],
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

        extensions = try container.decodeIfPresent([String].self, forKey: .extensions) ?? []
        subTargets = try container.decodeIfPresent([SubTarget].self, forKey: .subTargets) ?? []
        sharedTables = try container.decodeIfPresent([String].self, forKey: .sharedTables) ?? []
        components = try container.decodeIfPresent([ProjectComponentAttachment].self, forKey: .components) ?? []

        remoteFiles = try container.decodeIfPresent(RemoteFileConfig.self, forKey: .remoteFiles)
            ?? RemoteFileConfig()

        tools = try container.decodeIfPresent(ToolsConfig.self, forKey: .tools)
            ?? ToolsConfig()

        componentIds = try container.decodeIfPresent([String].self, forKey: .componentIds)
            ?? components.map { $0.id }
        componentOverrides = try container.decodeIfPresent([String: ProjectComponentOverrides].self, forKey: .componentOverrides) ?? [:]
        services = try container.decodeIfPresent([String].self, forKey: .services) ?? []
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
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

        try container.encode(extensions, forKey: .extensions)
        try container.encode(remoteFiles, forKey: .remoteFiles)
        try container.encode(tools, forKey: .tools)

        try container.encode(subTargets, forKey: .subTargets)
        try container.encode(sharedTables, forKey: .sharedTables)
        try container.encode(components, forKey: .components)
        try container.encode(componentIds, forKey: .componentIds)
        try container.encode(componentOverrides, forKey: .componentOverrides)
        try container.encode(services, forKey: .services)
        try container.encodeIfPresent(changelogNextSectionLabel, forKey: .changelogNextSectionLabel)
        try container.encodeIfPresent(changelogNextSectionAliases, forKey: .changelogNextSectionAliases)
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
