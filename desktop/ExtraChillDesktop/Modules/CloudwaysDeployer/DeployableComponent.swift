import Foundation

enum ComponentType: String, CaseIterable {
    case theme = "Theme"
    case networkPlugin = "Network Plugins"
    case sitePlugin = "Site Plugins"
    case dataMachine = "Data Machine"
}

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
    let localBasePath: String
    let localRelativePath: String
    let mainFile: String
    let remotePath: String
    
    var localFullPath: String {
        "\(localBasePath)/\(localRelativePath)"
    }
    
    var buildScriptPath: String {
        "\(localFullPath)/build.sh"
    }
    
    var buildOutputPath: String {
        "\(localFullPath)/build/\(id).zip"
    }
    
    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }
    
    static func == (lhs: DeployableComponent, rhs: DeployableComponent) -> Bool {
        lhs.id == rhs.id
    }
}

struct ComponentRegistry {
    static var extraChillBasePath: String {
        UserDefaults.standard.string(forKey: "extraChillBasePath") ?? "/Users/chubes/Developer/Extra Chill Platform"
    }
    
    static var dataMachineBasePath: String {
        UserDefaults.standard.string(forKey: "dataMachineBasePath") ?? "/Users/chubes/Developer/Data Machine Ecosystem"
    }
    
    static var all: [DeployableComponent] {
        [
        // Theme
        DeployableComponent(
            id: "extrachill",
            name: "ExtraChill Theme",
            type: .theme,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill",
            mainFile: "style.css",
            remotePath: "themes/extrachill"
        ),
        
        // Network Plugins
        DeployableComponent(
            id: "extrachill-multisite",
            name: "ExtraChill Multisite",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-multisite",
            mainFile: "extrachill-multisite.php",
            remotePath: "plugins/extrachill-multisite"
        ),
        DeployableComponent(
            id: "extrachill-users",
            name: "ExtraChill Users",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-users",
            mainFile: "extrachill-users.php",
            remotePath: "plugins/extrachill-users"
        ),
        DeployableComponent(
            id: "extrachill-ai-client",
            name: "ExtraChill AI Client",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-ai-client",
            mainFile: "extrachill-ai-client.php",
            remotePath: "plugins/extrachill-ai-client"
        ),
        DeployableComponent(
            id: "extrachill-api",
            name: "ExtraChill API",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-api",
            mainFile: "extrachill-api.php",
            remotePath: "plugins/extrachill-api"
        ),
        DeployableComponent(
            id: "extrachill-search",
            name: "ExtraChill Search",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-search",
            mainFile: "extrachill-search.php",
            remotePath: "plugins/extrachill-search"
        ),
        DeployableComponent(
            id: "extrachill-newsletter",
            name: "ExtraChill Newsletter",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-newsletter",
            mainFile: "extrachill-newsletter.php",
            remotePath: "plugins/extrachill-newsletter"
        ),
        DeployableComponent(
            id: "extrachill-admin-tools",
            name: "ExtraChill Admin Tools",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-admin-tools",
            mainFile: "extrachill-admin-tools.php",
            remotePath: "plugins/extrachill-admin-tools"
        ),
        DeployableComponent(
            id: "extrachill-analytics",
            name: "ExtraChill Analytics",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-analytics",
            mainFile: "extrachill-analytics.php",
            remotePath: "plugins/extrachill-analytics"
        ),
        DeployableComponent(
            id: "extrachill-seo",
            name: "ExtraChill SEO",
            type: .networkPlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/network/extrachill-seo",
            mainFile: "extrachill-seo.php",
            remotePath: "plugins/extrachill-seo"
        ),
        
        // Site Plugins
        DeployableComponent(
            id: "extrachill-blog",
            name: "ExtraChill Blog",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-blog",
            mainFile: "extrachill-blog.php",
            remotePath: "plugins/extrachill-blog"
        ),
        DeployableComponent(
            id: "extrachill-docs",
            name: "ExtraChill Docs",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-docs",
            mainFile: "extrachill-docs.php",
            remotePath: "plugins/extrachill-docs"
        ),
        DeployableComponent(
            id: "extrachill-shop",
            name: "ExtraChill Shop",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-shop",
            mainFile: "extrachill-shop.php",
            remotePath: "plugins/extrachill-shop"
        ),

        DeployableComponent(
            id: "extrachill-artist-platform",
            name: "ExtraChill Artist Platform",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-artist-platform",
            mainFile: "extrachill-artist-platform.php",
            remotePath: "plugins/extrachill-artist-platform"
        ),
        DeployableComponent(
            id: "extrachill-community",
            name: "ExtraChill Community",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-community",
            mainFile: "extrachill-community.php",
            remotePath: "plugins/extrachill-community"
        ),
        DeployableComponent(
            id: "extrachill-events",
            name: "ExtraChill Events",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-events",
            mainFile: "extrachill-events.php",
            remotePath: "plugins/extrachill-events"
        ),
        DeployableComponent(
            id: "extrachill-news-wire",
            name: "ExtraChill News Wire",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-news-wire",
            mainFile: "extrachill-news-wire.php",
            remotePath: "plugins/extrachill-news-wire"
        ),
        DeployableComponent(
            id: "extrachill-contact",
            name: "ExtraChill Contact",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-contact",
            mainFile: "extrachill-contact.php",
            remotePath: "plugins/extrachill-contact"
        ),
        DeployableComponent(
            id: "extrachill-chat",
            name: "ExtraChill Chat",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-chat",
            mainFile: "extrachill-chat.php",
            remotePath: "plugins/extrachill-chat"
        ),
        DeployableComponent(
            id: "extrachill-horoscopes",
            name: "ExtraChill Horoscopes",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/extrachill-horoscopes",
            mainFile: "extrachill-horoscopes.php",
            remotePath: "plugins/extrachill-horoscopes"
        ),
        DeployableComponent(
            id: "blocks-everywhere",
            name: "Blocks Everywhere",
            type: .sitePlugin,
            localBasePath: extraChillBasePath,
            localRelativePath: "extrachill-plugins/blocks-everywhere",
            mainFile: "blocks-everywhere.php",
            remotePath: "plugins/blocks-everywhere"
        ),
        
        // Data Machine
        DeployableComponent(
            id: "data-machine",
            name: "Data Machine",
            type: .dataMachine,
            localBasePath: dataMachineBasePath,
            localRelativePath: "data-machine",
            mainFile: "data-machine.php",
            remotePath: "plugins/data-machine"
        ),
        DeployableComponent(
            id: "datamachine-events",
            name: "Data Machine Events",
            type: .dataMachine,
            localBasePath: dataMachineBasePath,
            localRelativePath: "datamachine-events",
            mainFile: "datamachine-events.php",
            remotePath: "plugins/datamachine-events"
        ),
        ]
    }
    
    static func grouped() -> [(type: ComponentType, components: [DeployableComponent])] {
        var result: [(type: ComponentType, components: [DeployableComponent])] = []
        for type in ComponentType.allCases {
            let components = all.filter { $0.type == type }
            if !components.isEmpty {
                result.append((type: type, components: components))
            }
        }
        return result
    }
}

// MARK: - Version Parsing

struct VersionParser {
    static func parseLocalVersion(for component: DeployableComponent) -> String? {
        let filePath = "\(component.localFullPath)/\(component.mainFile)"
        guard let content = try? String(contentsOfFile: filePath, encoding: .utf8) else {
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
}
