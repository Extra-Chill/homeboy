import Foundation

enum DeployStatus: Equatable {
    case current
    case needsUpdate
    case missing
    case unknown
    case deploying
    case failed(String)
    
    var icon: String {
        switch self {
        case .current: return "checkmark.circle.fill"
        case .needsUpdate: return "arrow.up.circle.fill"
        case .missing: return "xmark.circle.fill"
        case .unknown: return "questionmark.circle"
        case .deploying: return "arrow.triangle.2.circlepath"
        case .failed: return "exclamationmark.triangle.fill"
        }
    }
    
    var color: String {
        switch self {
        case .current: return "green"
        case .needsUpdate: return "orange"
        case .missing: return "red"
        case .unknown: return "gray"
        case .deploying: return "blue"
        case .failed: return "red"
        }
    }
}

struct DeployableComponent: Identifiable, Hashable {
    let id: String
    let name: String
    let type: ComponentType
    let localPath: String
    let isNetwork: Bool
    
    var mainFile: String {
        type == .theme ? "style.css" : "\(id).php"
    }
    
    var remotePath: String {
        type == .theme ? "themes/\(id)" : "plugins/\(id)"
    }
    
    var mainFilePath: String {
        "\(localPath)/\(mainFile)"
    }
    
    var buildScriptPath: String {
        "\(localPath)/build.sh"
    }
    
    var buildOutputPath: String {
        "\(localPath)/build/\(id).zip"
    }
    
    init(from config: ComponentConfig) {
        self.id = config.id
        self.name = config.name
        self.type = config.type
        self.localPath = config.localPath
        self.isNetwork = config.isNetwork
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
    
    static func grouped() -> [(title: String, components: [DeployableComponent])] {
        let allComponents = all
        var result: [(title: String, components: [DeployableComponent])] = []
        
        let themes = allComponents.filter { $0.type == .theme }
        if !themes.isEmpty {
            result.append((title: "Themes", components: themes))
        }
        
        let networkPlugins = allComponents.filter { $0.type == .plugin && $0.isNetwork }
        if !networkPlugins.isEmpty {
            result.append((title: "Network Plugins", components: networkPlugins))
        }
        
        let sitePlugins = allComponents.filter { $0.type == .plugin && !$0.isNetwork }
        if !sitePlugins.isEmpty {
            result.append((title: "Site Plugins", components: sitePlugins))
        }
        
        return result
    }
}

// MARK: - Version & Header Parsing

struct VersionParser {
    static func parseLocalVersion(for component: DeployableComponent) -> String? {
        guard let content = try? String(contentsOfFile: component.mainFilePath, encoding: .utf8) else {
            return nil
        }
        return parseVersion(from: content)
    }
    
    static func parseVersion(from content: String) -> String? {
        let pattern = "Version:\\s*([0-9]+\\.[0-9]+\\.?[0-9]*)"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive) else {
            return nil
        }
        
        let range = NSRange(content.startIndex..., in: content)
        guard let match = regex.firstMatch(in: content, options: [], range: range) else {
            return nil
        }
        
        guard let versionRange = Range(match.range(at: 1), in: content) else {
            return nil
        }
        
        return String(content[versionRange]).trimmingCharacters(in: .whitespaces)
    }
    
    static func parsePluginName(from filePath: String) -> String? {
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else {
            return nil
        }
        return parseHeader(key: "Plugin Name", from: content)
    }
    
    static func parseThemeName(from filePath: String) -> String? {
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else {
            return nil
        }
        return parseHeader(key: "Theme Name", from: content)
    }
    
    private static func parseHeader(key: String, from content: String) -> String? {
        let pattern = "\(key):\\s*(.+)"
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive) else {
            return nil
        }
        
        let range = NSRange(content.startIndex..., in: content)
        guard let match = regex.firstMatch(in: content, options: [], range: range),
              let valueRange = Range(match.range(at: 1), in: content) else {
            return nil
        }
        
        return String(content[valueRange]).trimmingCharacters(in: .whitespaces)
    }
}
