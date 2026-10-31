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
    
    /// Creates context from current active project and server configuration.
    /// Must be called from main thread (UI code).
    static func current(source: String, additionalInfo: [String: String] = [:]) -> ContentContext {
        // Error creation always happens from UI code on main thread
        MainActor.assumeIsolated {
            let project = ConfigurationManager.shared.activeProject
            let server = ConfigurationManager.shared.serverForActiveProject()

            return ContentContext(
                source: source,
                projectName: project?.name ?? project?.id,
                serverName: server?.name ?? server?.id,
                serverHost: server?.host,
                additionalInfo: additionalInfo
            )
        }
    }

    /// Creates context with a specific path included.
    /// Must be called from main thread (UI code).
    static func current(source: String, path: String?, additionalInfo: [String: String] = [:]) -> ContentContext {
        var info = additionalInfo
        if let path = path, !path.isEmpty {
            info["Path"] = path
        }
        return current(source: source, additionalInfo: info)
    }
}
