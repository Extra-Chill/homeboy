import Foundation

/// Centralized application directory paths - Single Source of Truth
enum AppPaths {
    static let appName = "Homeboy"

    static var appSupport: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    }

    static var homeboy: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config")
            .appendingPathComponent("homeboy")
    }

    static var projects: URL {
        homeboy.appendingPathComponent("projects")
    }

    static var servers: URL {
        homeboy.appendingPathComponent("servers")
    }

    static var components: URL {
        homeboy.appendingPathComponent("components")
    }

    static var modules: URL {
        homeboy.appendingPathComponent("modules")
    }

    static var keys: URL {
        homeboy.appendingPathComponent("keys")
    }

    static var backups: URL {
        homeboy.appendingPathComponent("backups")
    }

    static var venv: URL {
        homeboy.appendingPathComponent("venv")
    }

    static var playwrightBrowsers: URL {
        homeboy.appendingPathComponent("playwright-browsers")
    }

    static var projectTypes: URL {
        homeboy.appendingPathComponent("project-types")
    }

    static func project(id: String) -> URL {
        projects.appendingPathComponent("\(id).json")
    }

    static func server(id: String) -> URL {
        servers.appendingPathComponent("\(id).json")
    }

    static func component(id: String) -> URL {
        components.appendingPathComponent("\(id).json")
    }

    static func module(id: String) -> URL {
        modules.appendingPathComponent(id)
    }

    static func key(forServer serverId: String) -> URL {
        keys.appendingPathComponent("\(serverId)_id_rsa")
    }

    static func backup(forProject projectId: String) -> URL {
        backups.appendingPathComponent(projectId)
    }
}
