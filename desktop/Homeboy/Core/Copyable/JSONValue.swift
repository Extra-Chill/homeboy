import Foundation

/// Type-erased JSON value for parsing CLI error details.
/// Preserves the structure of arbitrary JSON while allowing string representation for display.
enum JSONValue: Decodable, Sendable, Equatable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case null
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()

        if container.decodeNil() {
            self = .null
            return
        }

        if let string = try? container.decode(String.self) {
            self = .string(string)
            return
        }

        if let int = try? container.decode(Int.self) {
            self = .int(int)
            return
        }

        if let double = try? container.decode(Double.self) {
            self = .double(double)
            return
        }

        if let bool = try? container.decode(Bool.self) {
            self = .bool(bool)
            return
        }

        if let array = try? container.decode([JSONValue].self) {
            self = .array(array)
            return
        }

        if let object = try? container.decode([String: JSONValue].self) {
            self = .object(object)
            return
        }

        throw DecodingError.typeMismatch(
            JSONValue.self,
            DecodingError.Context(
                codingPath: decoder.codingPath,
                debugDescription: "Unable to decode value"
            )
        )
    }

    /// Flatten value to string for display in error context
    var stringValue: String {
        switch self {
        case .string(let value):
            return value
        case .int(let value):
            return String(value)
        case .double(let value):
            return String(value)
        case .bool(let value):
            return value ? "true" : "false"
        case .null:
            return "null"
        case .array(let values):
            let items = values.map { $0.stringValue }
            return "[\(items.joined(separator: ", "))]"
        case .object(let dict):
            let pairs = dict.map { "\($0.key): \($0.value.stringValue)" }
            return "{\(pairs.joined(separator: ", "))}"
        }
    }

    /// Flatten nested object to key-value pairs with dotted keys for error context.
    /// For example: {"target": {"host": "example.com"}} becomes ["target.host": "example.com"]
    func flattenedKeyValues(prefix: String = "") -> [String: String] {
        switch self {
        case .object(let dict):
            var result: [String: String] = [:]
            for (key, value) in dict {
                let fullKey = prefix.isEmpty ? key : "\(prefix).\(key)"
                let nested = value.flattenedKeyValues(prefix: fullKey)
                if nested.isEmpty {
                    result[fullKey] = value.stringValue
                } else {
                    result.merge(nested) { _, new in new }
                }
            }
            return result
        case .array:
            return [prefix: stringValue]
        default:
            return [:]
        }
    }
}
