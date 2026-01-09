import Foundation

/// Defines a project type with its display properties and capabilities.
/// Project types can be built-in (shipped with the app) or user-defined (JSON files).
///
/// Capabilities are inferred from configuration blocks:
/// - CLI available if `cli` block exists
/// - Database schema (table prefixes, groupings) available if `database` block exists
/// - All core tools (Deployer, File Editor, Log Viewer, Database Browser) are universal
struct ProjectTypeDefinition: Codable, Identifiable, Equatable {
    let id: String
    let displayName: String
    let icon: String
    let configSchema: String?
    let defaultPinnedFiles: [String]
    let defaultPinnedLogs: [String]
    let database: DatabaseSchemaDefinition?
    let cli: CLIConfig?
    let discovery: DiscoveryConfig?
    
    /// CLI is available if a cli configuration block exists
    var hasCLI: Bool { cli != nil }
    
    /// Discovery is available if a discovery configuration block exists
    var hasDiscovery: Bool { discovery != nil }
    
    /// Whether this is a WordPress project type
    var isWordPress: Bool { configSchema == "wordpress" }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        displayName = try container.decode(String.self, forKey: .displayName)
        icon = try container.decode(String.self, forKey: .icon)
        configSchema = try container.decodeIfPresent(String.self, forKey: .configSchema)
        defaultPinnedFiles = try container.decodeIfPresent([String].self, forKey: .defaultPinnedFiles) ?? []
        defaultPinnedLogs = try container.decodeIfPresent([String].self, forKey: .defaultPinnedLogs) ?? []
        database = try container.decodeIfPresent(DatabaseSchemaDefinition.self, forKey: .database)
        cli = try container.decodeIfPresent(CLIConfig.self, forKey: .cli)
        discovery = try container.decodeIfPresent(DiscoveryConfig.self, forKey: .discovery)
    }
}

extension ProjectTypeDefinition {
    
    /// Fallback generic project type when no JSON definition is found
    static let fallbackGeneric = ProjectTypeDefinition(
        id: "generic",
        displayName: "Generic",
        icon: "server.rack",
        configSchema: nil,
        defaultPinnedFiles: [],
        defaultPinnedLogs: [],
        database: nil,
        cli: nil,
        discovery: nil
    )
    
    init(
        id: String,
        displayName: String,
        icon: String,
        configSchema: String?,
        defaultPinnedFiles: [String],
        defaultPinnedLogs: [String],
        database: DatabaseSchemaDefinition? = nil,
        cli: CLIConfig? = nil,
        discovery: DiscoveryConfig? = nil
    ) {
        self.id = id
        self.displayName = displayName
        self.icon = icon
        self.configSchema = configSchema
        self.defaultPinnedFiles = defaultPinnedFiles
        self.defaultPinnedLogs = defaultPinnedLogs
        self.database = database
        self.cli = cli
        self.discovery = discovery
    }
}

// MARK: - Database Schema Definition

/// Defines database-related schema for a project type.
/// Contains CLI commands, table suffixes, protection patterns, and grouping templates.
struct DatabaseSchemaDefinition: Codable, Equatable {
    let cli: DatabaseCLIConfig?
    let defaultTablePrefix: String?
    let prefixDetectionSuffixes: [String]?
    let tableSuffixes: [String: [String]]?
    let protectedSuffixes: [String]?
    let defaultGrouping: GroupingTemplate?
    let multisiteGrouping: MultisiteGroupingTemplate?
}

/// Defines CLI command templates for database operations.
/// Used by `homeboy db` to execute database commands on remote servers.
struct DatabaseCLIConfig: Codable, Equatable {
    let tablesCommand: String
    let describeCommand: String
    let queryCommand: String
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

// MARK: - CLI Configuration

/// Defines the CLI tool configuration for a project type.
/// Used by the Homeboy CLI for both remote (via SSH) and local execution.
struct CLIConfig: Codable, Equatable {
    let tool: String              // CLI tool identifier (e.g., "wp", "pm2", "artisan")
    let displayName: String       // Human-readable name (e.g., "WP-CLI", "PM2")
    let commandTemplate: String   // Command template with {{variables}}
    let defaultCLIPath: String?   // Default path to CLI binary (e.g., "wp", "/opt/homebrew/bin/wp")
    
    private enum CodingKeys: String, CodingKey {
        case tool, displayName, commandTemplate, defaultCLIPath
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        tool = try container.decode(String.self, forKey: .tool)
        displayName = try container.decode(String.self, forKey: .displayName)
        commandTemplate = try container.decode(String.self, forKey: .commandTemplate)
        defaultCLIPath = try container.decodeIfPresent(String.self, forKey: .defaultCLIPath)
    }
    
    init(tool: String, displayName: String, commandTemplate: String, defaultCLIPath: String? = nil) {
        self.tool = tool
        self.displayName = displayName
        self.commandTemplate = commandTemplate
        self.defaultCLIPath = defaultCLIPath
    }
}

// MARK: - Discovery Configuration

/// Defines how to discover project installations on a remote server.
/// Used by `homeboy project discover` to find and set the basePath.
struct DiscoveryConfig: Codable, Equatable {
    /// Command to find candidate installations (e.g., "find /home -name 'wp-config.php' -type f 2>/dev/null")
    let findCommand: String
    
    /// How to transform find results into basePath ("dirname" or "identity")
    let basePathTransform: String
    
    /// Optional command to get human-readable name for each installation.
    /// Supports {{basePath}} template variable.
    let displayNameCommand: String?
    
    /// Apply the basePathTransform to a found path
    func transformToBasePath(_ path: String) -> String {
        switch basePathTransform {
        case "dirname":
            return (path as NSString).deletingLastPathComponent
        default:
            return path
        }
    }
}
