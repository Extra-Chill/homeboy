import Foundation

enum DeployStatus: Equatable {
    case current
    case needsUpdate
    case notDeployed
    case buildRequired
    case unknown
    case deploying
    case failed(String)
    
    var icon: String {
        switch self {
        case .current: return "checkmark.circle.fill"
        case .needsUpdate: return "arrow.up.circle.fill"
        case .notDeployed: return "xmark.circle.fill"
        case .buildRequired: return "hammer.fill"
        case .unknown: return "questionmark.circle"
        case .deploying: return "arrow.triangle.2.circlepath"
        case .failed: return "exclamationmark.triangle.fill"
        }
    }
    
    var color: String {
        switch self {
        case .current: return "green"
        case .needsUpdate: return "orange"
        case .notDeployed: return "red"
        case .buildRequired: return "yellow"
        case .unknown: return "gray"
        case .deploying: return "blue"
        case .failed: return "red"
        }
    }
}

struct DeployableComponent: Identifiable, Hashable {
    let id: String
    let name: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String
    let versionFile: String?
    let versionPattern: String?
    let isNetwork: Bool
    
    var buildArtifactPath: String {
        "\(localPath)/\(buildArtifact)"
    }
    
    var versionFilePath: String? {
        guard let vf = versionFile else { return nil }
        return "\(localPath)/\(vf)"
    }
    
    var artifactExtension: String {
        (buildArtifact as NSString).pathExtension.lowercased()
    }
    
    var hasBuildArtifact: Bool {
        FileManager.default.fileExists(atPath: buildArtifactPath)
    }
    
    /// Auto-detect if this is a network plugin by parsing the plugin header.
    /// Returns the stored isNetwork value if detection fails or for non-plugins.
    var isNetworkPlugin: Bool {
        if let versionPath = versionFilePath {
            let detected = VersionParser.parseNetworkFlag(from: versionPath)
            if detected { return true }
        }
        return isNetwork
    }
    
    init(from config: ComponentConfig) {
        self.id = config.id
        self.name = config.name
        self.localPath = config.localPath
        self.remotePath = config.remotePath
        self.buildArtifact = config.buildArtifact
        self.versionFile = config.versionFile
        self.versionPattern = config.versionPattern
        self.isNetwork = config.isNetwork ?? false
    }
    
    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }
    
    static func == (lhs: DeployableComponent, rhs: DeployableComponent) -> Bool {
        lhs.id == rhs.id
    }
}

struct ComponentRegistry {
    static var all: [DeployableComponent] {
        ConfigurationManager.readCurrentProject().components.map { DeployableComponent(from: $0) }
    }
}

// MARK: - Version Info

enum VersionInfo: Equatable {
    case version(String)
    case timestamp(Date)
    case notDeployed
    
    var displayString: String {
        switch self {
        case .version(let v):
            return v
        case .timestamp(let date):
            let formatter = DateFormatter()
            formatter.dateStyle = .medium
            formatter.timeStyle = .none
            return formatter.string(from: date)
        case .notDeployed:
            return "—"
        }
    }
}

// MARK: - Version Parsing

struct VersionParser {
    
    /// Default WordPress version pattern
    static let wordPressVersionPattern = "Version:\\s*([0-9]+\\.[0-9]+\\.?[0-9]*)"
    
    /// Parse version from local file for a component
    static func parseLocalVersion(for component: DeployableComponent) -> String? {
        guard let filePath = component.versionFilePath,
              let content = try? String(contentsOfFile: filePath, encoding: .utf8) else {
            return nil
        }
        return parseVersion(from: content, pattern: component.versionPattern)
    }
    
    /// Parse version from content using optional custom pattern
    static func parseVersion(from content: String, pattern: String? = nil) -> String? {
        let regexPattern = pattern ?? wordPressVersionPattern
        guard let regex = try? NSRegularExpression(pattern: regexPattern, options: .caseInsensitive) else {
            return nil
        }
        
        let range = NSRange(content.startIndex..., in: content)
        guard let match = regex.firstMatch(in: content, options: [], range: range),
              match.numberOfRanges > 1,
              let versionRange = Range(match.range(at: 1), in: content) else {
            return nil
        }
        
        return String(content[versionRange]).trimmingCharacters(in: .whitespaces)
    }
    
    /// Parse WordPress plugin header for "Network: true" flag.
    /// This indicates the plugin is network-activated in WordPress multisite.
    static func parseNetworkFlag(from filePath: String) -> Bool {
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else {
            return false
        }
        
        // Look for "Network: true" or "Network: True" in plugin header
        let pattern = "Network:\\s*(true|yes)"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive) else {
            return false
        }
        
        let range = NSRange(content.startIndex..., in: content)
        return regex.firstMatch(in: content, options: [], range: range) != nil
    }
}
