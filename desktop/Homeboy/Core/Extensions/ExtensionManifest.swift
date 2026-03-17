import Foundation

/// Represents a extension's manifest (extension.json)
/// Updated for grouped capability structure (executable/platform)
struct ExtensionManifest: Codable, Identifiable {
    let id: String
    let name: String
    let version: String
    let icon: String
    let description: String
    let author: String
    let homepage: String?

    // Grouped capabilities (NEW structure)
    let executable: ExecutableConfig?      // Was: runtime, inputs, output
    let platform: PlatformConfig?          // Was: config_schema, pinned files/logs, database, discovery, commands

    // Top-level capabilities (unchanged)
    let cli: CLITaskConfig?
    let build: CLITaskConfig?
    let lint: CLITaskConfig?
    let test: CLITaskConfig?
    let actions: [ActionConfig]?
    let hooks: HooksConfig?
    let settings: [SettingConfig]?
    let requires: RequirementsConfig?
    let extra: [String: String]?

    /// Path to the extension directory (set after loading, not from JSON)
    var extensionPath: String?

    // MARK: - Backward compatibility accessors

    /// Runtime config (from executable group)
    var runtime: RuntimeConfig? { executable?.runtime }

    /// Inputs (from executable group)
    var inputs: [InputConfig]? { executable?.inputs }

    /// Output (from executable group)
    var output: OutputConfig? { executable?.output }

    /// Config schema (from platform group)
    var configSchema: [String: ConfigSchemaItem]? { platform?.configSchema }

    /// Default pinned files (from platform group)
    var defaultPinnedFiles: [String]? { platform?.defaultPinnedFiles }

    /// Default pinned logs (from platform group)
    var defaultPinnedLogs: [String]? { platform?.defaultPinnedLogs }

    /// Database config (from platform group)
    var database: DatabaseConfig? { platform?.database }

    /// Discovery config (from platform group)
    var discovery: DiscoveryConfig? { platform?.discovery }

    /// Platform commands (from platform group)
    var commands: [PlatformCommand]? { platform?.commands }
}

// MARK: - Requirements Configuration

struct RequirementsConfig: Codable {
    let components: [String]?
    let features: [String]?
    let projectType: String?
}

// MARK: - Runtime Configuration

struct RuntimeConfig: Codable {
    let type: RuntimeType
    let entrypoint: String?
    let dependencies: [String]?
    let playwrightBrowsers: [String]?
    
    // CLI extension fields
    let args: String?
    let defaultSite: String?
    
    enum RuntimeType: String, Codable {
        case python
        case shell
        case cli
    }
}

// MARK: - Input Configuration

struct InputConfig: Codable, Identifiable {
    let id: String
    let type: InputType
    let label: String
    let placeholder: String?
    let `default`: InputDefault?
    let min: Int?
    let max: Int?
    let options: [SelectOption]?
    let arg: String
    
    enum InputType: String, Codable {
        case text
        case stepper
        case toggle
        case select
    }
}

/// Handles mixed-type defaults (string, int, bool)
enum InputDefault: Codable, Equatable {
    case string(String)
    case int(Int)
    case bool(Bool)
    
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let boolValue = try? container.decode(Bool.self) {
            self = .bool(boolValue)
        } else if let intValue = try? container.decode(Int.self) {
            self = .int(intValue)
        } else if let stringValue = try? container.decode(String.self) {
            self = .string(stringValue)
        } else {
            throw DecodingError.typeMismatch(
                InputDefault.self,
                DecodingError.Context(codingPath: decoder.codingPath, debugDescription: "Expected String, Int, or Bool")
            )
        }
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .int(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        }
    }
    
    var stringValue: String {
        switch self {
        case .string(let v): return v
        case .int(let v): return String(v)
        case .bool(let v): return v ? "true" : "false"
        }
    }
    
    var intValue: Int {
        switch self {
        case .int(let v): return v
        case .string(let v): return Int(v) ?? 0
        case .bool(let v): return v ? 1 : 0
        }
    }
    
    var boolValue: Bool {
        switch self {
        case .bool(let v): return v
        case .int(let v): return v != 0
        case .string(let v): return v.lowercased() == "true" || v == "1"
        }
    }
}

struct SelectOption: Codable, Identifiable {
    let value: String
    let label: String
    
    var id: String { value }
}

// MARK: - Output Configuration

struct OutputConfig: Codable {
    let schema: OutputSchema
    let display: DisplayType
    let selectable: Bool
    
    enum DisplayType: String, Codable {
        case table
        case json
        case logOnly = "log-only"
    }
}

struct OutputSchema: Codable {
    let type: String
    let items: [String: String]?
    
    /// Returns the column names for table display
    var columns: [String] {
        items?.keys.sorted() ?? []
    }
}

// MARK: - Action Configuration

struct ActionConfig: Codable, Identifiable {
    let id: String
    let label: String
    let type: ActionType
    
    // Builtin action properties
    let builtin: BuiltinAction?
    let column: String?
    
    // Command action properties
    let command: String?
    
    // API action properties
    let endpoint: String?
    let method: String?
    let requiresAuth: Bool?
    let payload: [String: PayloadValue]?
    
    enum ActionType: String, Codable {
        case builtin
        case api
        case command
    }
    
    enum BuiltinAction: String, Codable {
        case copyColumn = "copy-column"
        case exportCsv = "export-csv"
        case copyJson = "copy-json"
    }
}

/// Handles payload values which can be strings or interpolation templates
enum PayloadValue: Codable, Equatable {
    case string(String)
    case array([[String: String]])
    
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let stringValue = try? container.decode(String.self) {
            self = .string(stringValue)
        } else if let arrayValue = try? container.decode([[String: String]].self) {
            self = .array(arrayValue)
        } else {
            throw DecodingError.typeMismatch(
                PayloadValue.self,
                DecodingError.Context(codingPath: decoder.codingPath, debugDescription: "Expected String or Array")
            )
        }
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        }
    }
}

// MARK: - Setting Configuration

struct SettingConfig: Codable, Identifiable {
    let id: String
    let type: SettingType
    let label: String
    let placeholder: String?
    let `default`: InputDefault?
    
    enum SettingType: String, Codable {
        case text
        case string
        case toggle
        case stepper
    }
}


// MARK: - Script Output

/// The expected JSON output format from extension scripts
struct ScriptOutput: Codable {
    let success: Bool
    let results: [[String: AnyCodableValue]]?
    let errors: [String]?
}

/// Handles arbitrary JSON values in script results
enum AnyCodableValue: Codable, Equatable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case null
    
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let boolValue = try? container.decode(Bool.self) {
            self = .bool(boolValue)
        } else if let intValue = try? container.decode(Int.self) {
            self = .int(intValue)
        } else if let doubleValue = try? container.decode(Double.self) {
            self = .double(doubleValue)
        } else if let stringValue = try? container.decode(String.self) {
            self = .string(stringValue)
        } else {
            throw DecodingError.typeMismatch(
                AnyCodableValue.self,
                DecodingError.Context(codingPath: decoder.codingPath, debugDescription: "Unsupported type")
            )
        }
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .int(let value): try container.encode(value)
        case .double(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }
    
    var stringValue: String {
        switch self {
        case .string(let v): return v
        case .int(let v): return String(v)
        case .double(let v): return String(v)
        case .bool(let v): return v ? "true" : "false"
        case .null: return ""
        }
    }
}

// MARK: - Grouped Capability Configurations (NEW)

/// Executable extensions: Python/shell scripts with inputs and outputs
struct ExecutableConfig: Codable {
    let runtime: RuntimeConfig
    let inputs: [InputConfig]?
    let output: OutputConfig?
}

/// Platform extensions: WordPress/Node.js projects with discovery and commands
struct PlatformConfig: Codable {
    let configSchema: [String: ConfigSchemaItem]?
    let defaultPinnedFiles: [String]?
    let defaultPinnedLogs: [String]?
    let database: DatabaseConfig?
    let discovery: DiscoveryConfig?
    let commands: [PlatformCommand]?
}

/// Config schema item for platform extensions
struct ConfigSchemaItem: Codable {
    let type: String
    let description: String?
    let `default`: String?
    let required: Bool?
}

/// Database configuration for platform extensions
struct DatabaseConfig: Codable {
    let tables: [DatabaseTableConfig]?
    let views: [DatabaseViewConfig]?
}

struct DatabaseTableConfig: Codable {
    let name: String
    let description: String?
    let columns: [DatabaseColumnConfig]?
}

struct DatabaseViewConfig: Codable {
    let name: String
    let query: String
}

struct DatabaseColumnConfig: Codable {
    let name: String
    let type: String
    let description: String?
}

/// Discovery configuration for finding projects
struct DiscoveryConfig: Codable {
    let type: String  // e.g., "wordpress", "nodejs"
    let indicators: [String]?  // Files that indicate this project type
}

/// Platform-specific commands
struct PlatformCommand: Codable, Identifiable {
    let id: String
    let name: String
    let description: String?
    let command: String
}

/// CLI task configuration (build, lint, test)
struct CLITaskConfig: Codable {
    let command: String?
    let args: [String]?
    let timeout: Int?
}

/// Hooks configuration
struct HooksConfig: Codable {
    let preRun: [String]?
    let postRun: [String]?
}
