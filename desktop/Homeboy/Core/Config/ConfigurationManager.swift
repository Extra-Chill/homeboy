import Foundation
import SwiftUI

// MARK: - Project Change Notifications

extension Notification.Name {
    static let projectWillChange = Notification.Name("projectWillChange")
    static let projectDidChange = Notification.Name("projectDidChange")
}

/// Singleton manager for loading and saving JSON configuration files.
/// Each project is stored as a separate JSON file named by ID (e.g., extrachill.json).
@MainActor
class ConfigurationManager: ObservableObject {
    static let shared = ConfigurationManager()
    
    /// Thread-safe static accessor for reading project configuration from any context.
    nonisolated static func readCurrentProject() -> ProjectConfiguration {
        let fileManager = FileManager.default
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let configDir = appSupport.appendingPathComponent("Homeboy")
        let appConfigPath = configDir.appendingPathComponent("config.json")
        let projectsDir = configDir.appendingPathComponent("projects")
        
        guard let appData = try? Data(contentsOf: appConfigPath),
              let appConfig = try? JSONDecoder().decode(AppConfiguration.self, from: appData) else {
            return ProjectConfiguration.empty(id: "default", name: "Default", domain: "")
        }
        
        let projectPath = projectsDir.appendingPathComponent("\(appConfig.activeProjectId).json")
        guard let projectData = try? Data(contentsOf: projectPath),
              let project = try? JSONDecoder().decode(ProjectConfiguration.self, from: projectData) else {
            return ProjectConfiguration.empty(id: appConfig.activeProjectId, name: "Default", domain: "")
        }
        
        return project
    }
    
    @Published var appConfig: AppConfiguration
    @Published var activeProject: ProjectConfiguration
    
    /// Thread-safe accessor for reading project configuration from any context.
    nonisolated var currentProject: ProjectConfiguration {
        ConfigurationManager.readCurrentProject()
    }
    
    private let fileManager = FileManager.default
    private let jsonEncoder: JSONEncoder
    private let jsonDecoder: JSONDecoder
    
    private var configDirectory: URL {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy")
    }
    
    private var projectsDirectory: URL {
        configDirectory.appendingPathComponent("projects")
    }
    
    private var serversDirectory: URL {
        configDirectory.appendingPathComponent("servers")
    }
    
    private var appConfigPath: URL {
        configDirectory.appendingPathComponent("config.json")
    }
    
    // MARK: - Initialization
    
    private init() {
        jsonEncoder = JSONEncoder()
        jsonEncoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        jsonDecoder = JSONDecoder()
        
        appConfig = AppConfiguration()
        activeProject = ProjectConfiguration.empty(id: "default", name: "New Project", domain: "")
        
        ensureDirectoriesExist()
        load()
    }
    
    // MARK: - Directory Management
    
    private func ensureDirectoriesExist() {
        do {
            try fileManager.createDirectory(at: configDirectory, withIntermediateDirectories: true)
            try fileManager.createDirectory(at: projectsDirectory, withIntermediateDirectories: true)
            try fileManager.createDirectory(at: serversDirectory, withIntermediateDirectories: true)
        } catch {
            print("[ConfigurationManager] Failed to create directories: \(error)")
        }
    }
    
    // MARK: - Load Configuration
    
    private func load() {
        if fileManager.fileExists(atPath: appConfigPath.path) {
            loadFromJSON()
        } else {
            appConfig = AppConfiguration()
            activeProject = ProjectConfiguration.empty(id: "default", name: "New Project", domain: "")
            saveAppConfig()
            saveProject(activeProject)
        }
    }
    
    private func loadFromJSON() {
        do {
            let data = try Data(contentsOf: appConfigPath)
            appConfig = try jsonDecoder.decode(AppConfiguration.self, from: data)
        } catch {
            print("[ConfigurationManager] Failed to load app config: \(error)")
            appConfig = AppConfiguration()
        }
        
        if let project = loadProject(id: appConfig.activeProjectId) {
            activeProject = project
        } else {
            print("[ConfigurationManager] Active project '\(appConfig.activeProjectId)' not found, creating default")
            activeProject = ProjectConfiguration.empty(id: appConfig.activeProjectId, name: "New Project", domain: "")
            saveProject(activeProject)
        }
    }
    
    // MARK: - Save Configuration
    
    func saveAppConfig() {
        do {
            let data = try jsonEncoder.encode(appConfig)
            try data.write(to: appConfigPath)
        } catch {
            print("[ConfigurationManager] Failed to save app config: \(error)")
        }
    }
    
    func saveProject(_ project: ProjectConfiguration) {
        let projectPath = projectsDirectory.appendingPathComponent("\(project.id).json")
        do {
            let data = try jsonEncoder.encode(project)
            try data.write(to: projectPath)
        } catch {
            print("[ConfigurationManager] Failed to save project '\(project.id)': \(error)")
        }
    }
    
    func saveActiveProject() {
        saveProject(activeProject)
    }
    
    // MARK: - Load Project
    
    func loadProject(id: String) -> ProjectConfiguration? {
        let projectPath = projectsDirectory.appendingPathComponent("\(id).json")
        guard fileManager.fileExists(atPath: projectPath.path) else {
            return nil
        }
        
        do {
            let data = try Data(contentsOf: projectPath)
            return try jsonDecoder.decode(ProjectConfiguration.self, from: data)
        } catch {
            print("[ConfigurationManager] Failed to load project '\(id)': \(error)")
            return nil
        }
    }
    
    // MARK: - Project Management
    
    func availableProjectIds() -> [String] {
        do {
            let files = try fileManager.contentsOfDirectory(at: projectsDirectory, includingPropertiesForKeys: nil)
            return files
                .filter { $0.pathExtension == "json" }
                .map { $0.deletingPathExtension().lastPathComponent }
        } catch {
            print("[ConfigurationManager] Failed to list projects: \(error)")
            return []
        }
    }
    
    func switchToProject(id: String) {
        guard let project = loadProject(id: id) else {
            print("[ConfigurationManager] Cannot switch to project '\(id)': not found")
            return
        }
        
        NotificationCenter.default.post(name: .projectWillChange, object: nil)
        
        activeProject = project
        appConfig.activeProjectId = id
        saveAppConfig()
        
        NotificationCenter.default.post(name: .projectDidChange, object: nil)
    }
    
    func createProject(id: String, displayName: String, domain: String = "") -> ProjectConfiguration {
        let project = ProjectConfiguration.empty(id: id, name: displayName, domain: domain)
        saveProject(project)
        return project
    }
    
    func deleteProject(id: String) {
        guard id != appConfig.activeProjectId else {
            print("[ConfigurationManager] Cannot delete active project")
            return
        }
        
        let projectPath = projectsDirectory.appendingPathComponent("\(id).json")
        try? fileManager.removeItem(at: projectPath)
    }
    
    // MARK: - Server Management
    
    /// Thread-safe static accessor for reading a server configuration from any context.
    nonisolated static func readServer(id: String) -> ServerConfig? {
        let fileManager = FileManager.default
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let serverPath = appSupport
            .appendingPathComponent("Homeboy")
            .appendingPathComponent("servers")
            .appendingPathComponent("\(id).json")
        
        guard let data = try? Data(contentsOf: serverPath),
              let server = try? JSONDecoder().decode(ServerConfig.self, from: data) else {
            return nil
        }
        return server
    }
    
    /// Returns the server for the current active project
    nonisolated static func readCurrentServer() -> ServerConfig? {
        let project = readCurrentProject()
        guard let serverId = project.serverId else { return nil }
        return readServer(id: serverId)
    }
    
    func saveServer(_ server: ServerConfig) {
        let serverPath = serversDirectory.appendingPathComponent("\(server.id).json")
        do {
            let data = try jsonEncoder.encode(server)
            try data.write(to: serverPath)
        } catch {
            print("[ConfigurationManager] Failed to save server '\(server.id)': \(error)")
        }
    }
    
    func loadServer(id: String) -> ServerConfig? {
        let serverPath = serversDirectory.appendingPathComponent("\(id).json")
        guard fileManager.fileExists(atPath: serverPath.path) else {
            return nil
        }
        
        do {
            let data = try Data(contentsOf: serverPath)
            return try jsonDecoder.decode(ServerConfig.self, from: data)
        } catch {
            print("[ConfigurationManager] Failed to load server '\(id)': \(error)")
            return nil
        }
    }
    
    func availableServerIds() -> [String] {
        do {
            let files = try fileManager.contentsOfDirectory(at: serversDirectory, includingPropertiesForKeys: nil)
            return files
                .filter { $0.pathExtension == "json" }
                .map { $0.deletingPathExtension().lastPathComponent }
        } catch {
            print("[ConfigurationManager] Failed to list servers: \(error)")
            return []
        }
    }
    
    func availableServers() -> [ServerConfig] {
        availableServerIds().compactMap { loadServer(id: $0) }
    }
    
    func deleteServer(id: String) {
        // Check if any project references this server
        let projectsUsingServer = availableProjectIds().compactMap { loadProject(id: $0) }.filter { $0.serverId == id }
        guard projectsUsingServer.isEmpty else {
            print("[ConfigurationManager] Cannot delete server '\(id)': used by \(projectsUsingServer.count) project(s)")
            return
        }
        
        let serverPath = serversDirectory.appendingPathComponent("\(id).json")
        try? fileManager.removeItem(at: serverPath)
    }
    
    /// Returns the server for the active project
    func serverForActiveProject() -> ServerConfig? {
        guard let serverId = activeProject.serverId else { return nil }
        return loadServer(id: serverId)
    }
}
