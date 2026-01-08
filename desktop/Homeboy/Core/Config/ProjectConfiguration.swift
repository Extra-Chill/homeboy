import Foundation

// MARK: - Project Type

/// Defines the type of project for deployment strategy selection
enum ProjectType: String, Codable, CaseIterable {
    case wordpress
    
    var displayName: String { "WordPress" }
    var icon: String { "w.circle.fill" }
}

// MARK: - Project Configuration

/// Configuration for a single project (WordPress site, Node.js app, etc.)
struct ProjectConfiguration: Codable, Identifiable {
    var id: String              // Derived from domain/name, used as filename (e.g., "extrachill")
    var name: String            // Display name (e.g., "Extra Chill Platform")
    var domain: String          // Domain for web projects (e.g., "extrachill.com")
    var projectType: ProjectType // Type of project for deployment strategy
    
    var serverId: String?       // Reference to ServerConfig
    var basePath: String?       // Remote base path (e.g., "applications/extrachill/public_html")
    
    var database: DatabaseConfig
    var wordpress: WordPressConfig?     // Only for WordPress projects
    var localDev: LocalDevConfig
    var tools: ToolsConfig
    var api: APIConfig
    var multisite: MultisiteConfig?
    var components: [ComponentConfig]
    
    /// Creates a default empty project configuration
    static func empty(id: String, name: String, domain: String) -> ProjectConfiguration {
        ProjectConfiguration(
            id: id,
            name: name,
            domain: domain,
            projectType: .wordpress,
            serverId: nil,
            basePath: nil,
            database: DatabaseConfig(),
            wordpress: WordPressConfig(),
            localDev: LocalDevConfig(),
            tools: ToolsConfig(),
            api: APIConfig(),
            multisite: nil,
            components: []
        )
    }
}

// MARK: - Database Configuration

struct DatabaseConfig: Codable {
    var host: String
    var port: Int
    var name: String
    var user: String
    var useSSHTunnel: Bool
    
    init(host: String = "localhost", port: Int = 3306, name: String = "", user: String = "", useSSHTunnel: Bool = true) {
        self.host = host
        self.port = port
        self.name = name
        self.user = user
        self.useSSHTunnel = useSSHTunnel
    }
}

// MARK: - WordPress Configuration

/// WordPress-specific configuration (only used when projectType == .wordpress)
struct WordPressConfig: Codable {
    var wpContentPath: String   // User-provided path to wp-content directory (selected via file browser)
    
    init(wpContentPath: String = "") {
        self.wpContentPath = wpContentPath
    }
    
    /// Path to themes directory within wp-content
    var themesPath: String {
        wpContentPath.isEmpty ? "" : "\(wpContentPath)/themes"
    }
    
    /// Path to plugins directory within wp-content
    var pluginsPath: String {
        wpContentPath.isEmpty ? "" : "\(wpContentPath)/plugins"
    }
    
    /// Whether wp-content path is configured
    var isConfigured: Bool {
        !wpContentPath.isEmpty
    }
}

// MARK: - Local Development Configuration

struct LocalDevConfig: Codable {
    var wpCliPath: String
    var domain: String
    
    init(wpCliPath: String = "", domain: String = "") {
        self.wpCliPath = wpCliPath
        self.domain = domain
    }
}

// MARK: - API Configuration

struct APIConfig: Codable {
    var enabled: Bool
    var baseURL: String
    
    init(enabled: Bool = false, baseURL: String = "") {
        self.enabled = enabled
        self.baseURL = baseURL
    }
}

// MARK: - Multisite Configuration

struct MultisiteConfig: Codable {
    var enabled: Bool
    var tablePrefix: String
    var blogs: [MultisiteBlog]
    var networkTables: [String]
    
    init(
        enabled: Bool = false,
        tablePrefix: String = "wp_",
        blogs: [MultisiteBlog] = [],
        networkTables: [String] = []
    ) {
        self.enabled = enabled
        self.tablePrefix = tablePrefix
        self.blogs = blogs
        self.networkTables = networkTables
    }
}

struct MultisiteBlog: Codable, Identifiable {
    var blogId: Int
    var name: String
    var domain: String
    
    var id: Int { blogId }
    
    func tablePrefix(basePrefix: String) -> String {
        blogId == 1 ? basePrefix : "\(basePrefix)\(blogId)_"
    }
}

// MARK: - Component Configuration

enum ComponentType: String, Codable {
    case theme
    case plugin
}

struct ComponentConfig: Codable, Identifiable {
    var id: String
    var name: String
    var type: ComponentType
    var localPath: String
    var isNetwork: Bool
    
    init(id: String, name: String, type: ComponentType, localPath: String, isNetwork: Bool = false) {
        self.id = id
        self.name = name
        self.type = type
        self.localPath = localPath
        self.isNetwork = isNetwork
    }
    
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
