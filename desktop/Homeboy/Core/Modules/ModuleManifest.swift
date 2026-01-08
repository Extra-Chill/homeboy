import Foundation

/// Represents a module's manifest (module.json)
struct ModuleManifest: Codable, Identifiable {
    let id: String
    let name: String
    let version: String
    let icon: String
    let description: String
    let author: String
    let homepage: String?
    
    let runtime: RuntimeConfig
    let inputs: [InputConfig]
    let output: OutputConfig
    let actions: [ActionConfig]
    let settings: [SettingConfig]
    let requires: RequirementsConfig?
    
    /// Path to the module directory (set after loading, not from JSON)
    var modulePath: String?
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
    
    // WP-CLI specific fields
    let command: String?
    let subcommand: String?
    let defaultSite: String?
    
    enum RuntimeType: String, Codable {
        case python
        case shell
        case wpcli
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
    
    // API action properties
    let endpoint: String?
    let method: String?
    let requiresAuth: Bool?
    let payload: [String: PayloadValue]?
    
    enum ActionType: String, Codable {
        case builtin
        case api
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
        case toggle
        case stepper
    }
}

// MARK: - Module Settings Storage

/// Stores a module's persisted settings values
struct ModuleSettings: Codable {
    var values: [String: SettingValue]
    
    init(values: [String: SettingValue] = [:]) {
        self.values = values
    }
    
    func string(for key: String) -> String? {
        if case .string(let v) = values[key] { return v }
        return nil
    }
    
    func int(for key: String) -> Int? {
        if case .int(let v) = values[key] { return v }
        return nil
    }
    
    func bool(for key: String) -> Bool? {
        if case .bool(let v) = values[key] { return v }
        return nil
    }
    
    mutating func set(_ key: String, value: String) {
        values[key] = .string(value)
    }
    
    mutating func set(_ key: String, value: Int) {
        values[key] = .int(value)
    }
    
    mutating func set(_ key: String, value: Bool) {
        values[key] = .bool(value)
    }
}

enum SettingValue: Codable, Equatable {
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
                SettingValue.self,
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
}

// MARK: - Script Output

/// The expected JSON output format from module scripts
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
