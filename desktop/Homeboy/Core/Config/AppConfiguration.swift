import Foundation

/// Global application configuration stored in config.json
struct AppConfiguration: Codable {
    var version: Int
    var activeProjectId: String
    
    init(version: Int = 1, activeProjectId: String = "default") {
        self.version = version
        self.activeProjectId = activeProjectId
    }
}
