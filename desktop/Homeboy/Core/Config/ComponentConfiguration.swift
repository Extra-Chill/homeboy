import Foundation

/// Version target matching CLI's VersionTarget struct
struct VersionTarget: Codable {
    var file: String
    var pattern: String?
}

/// Extension configuration scoped to a component
/// Mirrors CLI's ScopedExtensionConfig
/// Settings use [String: Any] for flexibility - extension defines its own schema
struct ScopedExtensionConfig: Codable {
    var version: String?
    var settings: [String: String]?  // Simplified: key-value string pairs
}

/// Include/exclude path scopes for a Homeboy command family.
struct CommandScopeConfig: Codable, Equatable {
    var include: [String]
    var exclude: [String]

    init(include: [String] = [], exclude: [String] = []) {
        self.include = include
        self.exclude = exclude
    }
}

/// Component command scopes matching CLI's ScopeConfig.
struct ScopeConfig: Codable, Equatable {
    var defaults: CommandScopeConfig?
    var audit: CommandScopeConfig?
    var lint: CommandScopeConfig?
    var test: CommandScopeConfig?
    var refactor: CommandScopeConfig?
    var deploy: CommandScopeConfig?
    var release: CommandScopeConfig?
    var fleet: CommandScopeConfig?
}

/// Server-side git deployment settings matching CLI's GitDeployConfig.
struct GitDeployConfig: Codable, Equatable {
    var remote: String
    var branch: String
    var postPull: [String]
    var tagPattern: String?

    init(remote: String = "origin", branch: String = "main", postPull: [String] = [], tagPattern: String? = nil) {
        self.remote = remote
        self.branch = branch
        self.postPull = postPull
        self.tagPattern = tagPattern
    }

    private enum CodingKeys: String, CodingKey {
        case remote, branch, postPull, tagPattern
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        remote = try container.decodeIfPresent(String.self, forKey: .remote) ?? "origin"
        branch = try container.decodeIfPresent(String.self, forKey: .branch) ?? "main"
        postPull = try container.decodeIfPresent([String].self, forKey: .postPull) ?? []
        tagPattern = try container.decodeIfPresent(String.self, forKey: .tagPattern)
    }
}

/// Standalone component configuration stored in ~/.config/homeboy/components/{id}.json
/// Components are reusable across projects - projects reference components by ID.
/// Aligned with CLI's Component struct (CLI is authoritative).
struct ComponentConfiguration: Codable, Identifiable {
    var id: String
    var aliases: [String]              // NEW: Multiple aliases for component
    var localPath: String
    var remotePath: String
    var buildArtifact: String?
    var buildCommand: String?
    var extensions: [String: ScopedExtensionConfig]?  // NEW: Extension configs by ID
    var versionTargets: [VersionTarget]?
    var changelogTarget: String?       // NEW: Dedicated changelog file path
    var changelogNextSectionLabel: String?
    var changelogNextSectionAliases: [String]?
    var hooks: [String: [String]]?     // NEW: Lifecycle hooks
    var extractCommand: String?
    var remoteOwner: String?
    var deployStrategy: String?
    var gitDeploy: GitDeployConfig?
    var remoteURL: String?
    var autoCleanup: Bool
    var docsDir: String?
    var docsDirs: [String]
    var scopes: ScopeConfig?

    init(
        id: String,
        aliases: [String] = [],
        localPath: String,
        remotePath: String,
        buildArtifact: String? = nil,
        buildCommand: String? = nil,
        extensions: [String: ScopedExtensionConfig]? = nil,
        versionTargets: [VersionTarget]? = nil,
        changelogTarget: String? = nil,
        changelogNextSectionLabel: String? = nil,
        changelogNextSectionAliases: [String]? = nil,
        hooks: [String: [String]]? = nil,
        extractCommand: String? = nil,
        remoteOwner: String? = nil,
        deployStrategy: String? = nil,
        gitDeploy: GitDeployConfig? = nil,
        remoteURL: String? = nil,
        autoCleanup: Bool = false,
        docsDir: String? = nil,
        docsDirs: [String] = [],
        scopes: ScopeConfig? = nil
    ) {
        self.id = id
        self.aliases = aliases
        self.localPath = localPath
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.buildCommand = buildCommand
        self.extensions = extensions
        self.versionTargets = versionTargets
        self.changelogTarget = changelogTarget
        self.changelogNextSectionLabel = changelogNextSectionLabel
        self.changelogNextSectionAliases = changelogNextSectionAliases
        self.hooks = hooks
        self.extractCommand = extractCommand
        self.remoteOwner = remoteOwner
        self.deployStrategy = deployStrategy
        self.gitDeploy = gitDeploy
        self.remoteURL = remoteURL
        self.autoCleanup = autoCleanup
        self.docsDir = docsDir
        self.docsDirs = docsDirs
        self.scopes = scopes
    }

    private enum CodingKeys: String, CodingKey {
        case id, aliases, localPath, remotePath, buildArtifact, buildCommand, extensions, versionTargets
        case changelogTarget, changelogNextSectionLabel, changelogNextSectionAliases, hooks
        case extractCommand, remoteOwner, deployStrategy, gitDeploy, remoteURL = "remoteUrl", autoCleanup
        case docsDir, docsDirs, scopes
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)

        id = try container.decode(String.self, forKey: .id)
        aliases = try container.decodeIfPresent([String].self, forKey: .aliases) ?? []
        localPath = try container.decode(String.self, forKey: .localPath)
        remotePath = try container.decode(String.self, forKey: .remotePath)
        buildArtifact = try container.decodeIfPresent(String.self, forKey: .buildArtifact)
        buildCommand = try container.decodeIfPresent(String.self, forKey: .buildCommand)
        extensions = try container.decodeIfPresent([String: ScopedExtensionConfig].self, forKey: .extensions)
        versionTargets = try container.decodeIfPresent([VersionTarget].self, forKey: .versionTargets)
        changelogTarget = try container.decodeIfPresent(String.self, forKey: .changelogTarget)
        changelogNextSectionLabel = try container.decodeIfPresent(String.self, forKey: .changelogNextSectionLabel)
        changelogNextSectionAliases = try container.decodeIfPresent([String].self, forKey: .changelogNextSectionAliases)
        hooks = try container.decodeIfPresent([String: [String]].self, forKey: .hooks)
        extractCommand = try container.decodeIfPresent(String.self, forKey: .extractCommand)
        remoteOwner = try container.decodeIfPresent(String.self, forKey: .remoteOwner)
        deployStrategy = try container.decodeIfPresent(String.self, forKey: .deployStrategy)
        gitDeploy = try container.decodeIfPresent(GitDeployConfig.self, forKey: .gitDeploy)
        remoteURL = try container.decodeIfPresent(String.self, forKey: .remoteURL)
        autoCleanup = try container.decodeIfPresent(Bool.self, forKey: .autoCleanup) ?? false
        docsDir = try container.decodeIfPresent(String.self, forKey: .docsDir)
        docsDirs = try container.decodeIfPresent([String].self, forKey: .docsDirs) ?? []
        scopes = try container.decodeIfPresent(ScopeConfig.self, forKey: .scopes)
    }

    /// Display name computed from id (e.g., "chubes-theme" -> "Chubes Theme")
    var displayName: String {
        id.split(separator: "-")
            .map { $0.capitalized }
            .joined(separator: " ")
    }

    /// First version file from version_targets (for backward compat with UI)
    var versionFile: String? {
        versionTargets?.first?.file
    }

    /// First version pattern from version_targets (for backward compat with UI)
    var versionPattern: String? {
        versionTargets?.first?.pattern
    }

    /// Get configured extension IDs
    var extensionIds: [String] {
        extensions.map { Array($0.keys) } ?? []
    }

    /// Check if component has a specific extension configured
    func hasExtension(_ id: String) -> Bool {
        extensions?.keys.contains(id) ?? false
    }
}
