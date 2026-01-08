import Foundation

/// Configuration for a reusable SSH server connection
struct ServerConfig: Codable, Identifiable {
    var id: String           // Unique identifier (e.g., "production-1")
    var name: String         // Display name (e.g., "Production Server")
    var host: String         // SSH host (e.g., "178.128.155.94")
    var user: String         // SSH username (e.g., "master_xyz")
    var port: Int            // SSH port (default: 22)
    
    init(id: String, name: String, host: String, user: String, port: Int = 22) {
        self.id = id
        self.name = name
        self.host = host
        self.user = user
        self.port = port
    }
    
    /// Keychain service name for this server's SSH key
    var keychainServiceName: String {
        "com.extrachill.homeboy.ssh.\(id)"
    }
    
    /// Creates a default empty server configuration
    static func empty(id: String = "", name: String = "") -> ServerConfig {
        ServerConfig(id: id, name: name, host: "", user: "", port: 22)
    }
    
    /// Generates an ID from a host string (e.g., "178.128.155.94" -> "server-178-128-155-94")
    static func generateId(from host: String) -> String {
        "server-" + host.replacingOccurrences(of: ".", with: "-")
    }
}
