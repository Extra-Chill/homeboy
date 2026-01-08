import Foundation

/// Metadata context for copyable content, providing debugging information
struct ContentContext {
    let source: String
    let timestamp: Date
    let projectName: String?
    let serverName: String?
    let serverHost: String?
    let additionalInfo: [String: String]
    
    // MARK: - App Info (from Bundle)
    
    static var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "Unknown"
    }
    
    static var appBuild: String {
        Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "Unknown"
    }
    
    static var appIdentifier: String {
        "Homeboy \(appVersion) (Build \(appBuild))"
    }
    
    // MARK: - Initialization
    
    init(
        source: String,
        projectName: String? = nil,
        serverName: String? = nil,
        serverHost: String? = nil,
        additionalInfo: [String: String] = [:]
    ) {
        self.source = source
        self.timestamp = Date()
        self.projectName = projectName
        self.serverName = serverName
        self.serverHost = serverHost
        self.additionalInfo = additionalInfo
    }
    
    /// Creates context from current active project and server configuration
    static func current(source: String, additionalInfo: [String: String] = [:]) -> ContentContext {
        let project = ConfigurationManager.readCurrentProject()
        let server = ConfigurationManager.readCurrentServer()
        
        return ContentContext(
            source: source,
            projectName: project.name,
            serverName: server?.name,
            serverHost: server?.host,
            additionalInfo: additionalInfo
        )
    }
    
    /// Creates context with a specific path included
    static func current(source: String, path: String?, additionalInfo: [String: String] = [:]) -> ContentContext {
        var info = additionalInfo
        if let path = path, !path.isEmpty {
            info["Path"] = path
        }
        return current(source: source, additionalInfo: info)
    }
}
