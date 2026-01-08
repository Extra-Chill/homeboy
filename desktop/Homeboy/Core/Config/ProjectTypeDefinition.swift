import Foundation

/// Defines a project type with its display properties and available features.
/// Project types can be built-in (shipped with the app) or user-defined (JSON files).
struct ProjectTypeDefinition: Codable, Identifiable, Equatable {
    let id: String
    let displayName: String
    let icon: String
    let features: Set<String>
    let configSchema: String?
    let defaultPinnedFiles: [String]
    let defaultPinnedLogs: [String]
    let database: DatabaseSchemaDefinition?
    
    static let featureDeployer = "deployer"
    static let featureDebugLogs = "debugLogs"
    static let featureConfigEditor = "configEditor"
    static let featureDatabaseBrowser = "databaseBrowser"
    
    var hasDeployer: Bool { features.contains(Self.featureDeployer) }
    var hasDebugLogs: Bool { features.contains(Self.featureDebugLogs) }
    var hasConfigEditor: Bool { features.contains(Self.featureConfigEditor) }
    var hasDatabaseBrowser: Bool { features.contains(Self.featureDatabaseBrowser) }
    var isWordPress: Bool { configSchema == "wordpress" }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        displayName = try container.decode(String.self, forKey: .displayName)
        icon = try container.decode(String.self, forKey: .icon)
        features = try container.decode(Set<String>.self, forKey: .features)
        configSchema = try container.decodeIfPresent(String.self, forKey: .configSchema)
        defaultPinnedFiles = try container.decodeIfPresent([String].self, forKey: .defaultPinnedFiles) ?? []
        defaultPinnedLogs = try container.decodeIfPresent([String].self, forKey: .defaultPinnedLogs) ?? []
        database = try container.decodeIfPresent(DatabaseSchemaDefinition.self, forKey: .database)
    }
}

extension ProjectTypeDefinition {
    
    /// Fallback generic project type when no JSON definition is found
    static let fallbackGeneric = ProjectTypeDefinition(
        id: "generic",
        displayName: "Generic",
        icon: "server.rack",
        features: [featureDatabaseBrowser, featureConfigEditor],
        configSchema: nil,
        defaultPinnedFiles: [],
        defaultPinnedLogs: [],
        database: nil
    )
    
    init(
        id: String,
        displayName: String,
        icon: String,
        features: Set<String>,
        configSchema: String?,
        defaultPinnedFiles: [String],
        defaultPinnedLogs: [String],
        database: DatabaseSchemaDefinition? = nil
    ) {
        self.id = id
        self.displayName = displayName
        self.icon = icon
        self.features = features
        self.configSchema = configSchema
        self.defaultPinnedFiles = defaultPinnedFiles
        self.defaultPinnedLogs = defaultPinnedLogs
        self.database = database
    }
}

// MARK: - Database Schema Definition

/// Defines database-related schema for a project type.
/// Contains table suffixes, protection patterns, and grouping templates.
struct DatabaseSchemaDefinition: Codable, Equatable {
    let defaultTablePrefix: String?
    let prefixDetectionSuffixes: [String]?
    let tableSuffixes: [String: [String]]?
    let protectedSuffixes: [String]?
    let defaultGrouping: GroupingTemplate?
    let multisiteGrouping: MultisiteGroupingTemplate?
}

/// Template for generating a table grouping.
struct GroupingTemplate: Codable, Equatable {
    let id: String
    let name: String
    let patternTemplate: String
}

/// Template for generating multisite-specific groupings.
struct MultisiteGroupingTemplate: Codable, Equatable {
    let network: NetworkGroupingTemplate?
    let site: SiteGroupingTemplate?
}

/// Template for the network tables grouping.
struct NetworkGroupingTemplate: Codable, Equatable {
    let id: String
    let name: String
}

/// Template for per-site groupings in a multisite installation.
struct SiteGroupingTemplate: Codable, Equatable {
    let idTemplate: String
    let nameTemplate: String
    let patternTemplate: String
}
