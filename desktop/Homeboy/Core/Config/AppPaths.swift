import Foundation

enum AppPaths {
    static let homeboy = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".config")
        .appendingPathComponent("homeboy")

    static let projects = homeboy.appendingPathComponent("projects")
    static let servers = homeboy.appendingPathComponent("servers")
    static let components = homeboy.appendingPathComponent("components")
    static let modules = homeboy.appendingPathComponent("modules")
    static let keys = homeboy.appendingPathComponent("keys")
    static let backups = homeboy.appendingPathComponent("backups")

    static func project(id: String) -> URL {
        projects.appendingPathComponent("\(id).json")
    }

    static func server(id: String) -> URL {
        servers.appendingPathComponent("\(id).json")
    }

    static func component(id: String) -> URL {
        components.appendingPathComponent("\(id).json")
    }

    static func key(forServer serverId: String) -> URL {
        keys.appendingPathComponent("\(serverId)_id_rsa")
    }
}
