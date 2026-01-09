import Foundation
import SwiftUI

// MARK: - Project Change Notifications

extension Notification.Name {
    static let projectWillChange = Notification.Name("projectWillChange")
    static let projectDidChange = Notification.Name("projectDidChange")
}

// MARK: - Project Rename Errors

enum ProjectRenameError: LocalizedError {
    case nameCollision(existingName: String)
    case fileSystemError(Error)
    
    var errorDescription: String? {
        switch self {
        case .nameCollision(let existingName):
            return "A project named \"\(existingName)\" already exists"
        case .fileSystemError(let error):
            return "Failed to rename project file: \(error.localizedDescription)"
        }
    }
}

/// Singleton manager for loading and saving JSON configuration files.
/// Each project is stored as a separate JSON file named by ID (e.g., extrachill.json).
/// The project ID (slug) is derived from the project name and serves as the source of truth.
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
            return ProjectConfiguration.empty(id: "default", name: "Default")
        }
        
        let projectPath = projectsDir.appendingPathComponent("\(appConfig.activeProjectId).json")
        guard let projectData = try? Data(contentsOf: projectPath),
              let project = try? JSONDecoder().decode(ProjectConfiguration.self, from: projectData) else {
            return ProjectConfiguration.empty(id: appConfig.activeProjectId, name: "Default")
        }
        
        return project
    }
    
    @Published var appConfig: AppConfiguration
    @Published var activeProject: ProjectConfiguration?
    
    /// Whether the app needs the user to create a project (no projects exist or active project invalid)
    @Published var needsProjectCreation: Bool = false

    /// Reactive list of available projects (updates when projects are added/removed via CLI)
    @Published var availableProjects: [ProjectConfiguration] = []

    /// Reactive list of available servers (updates when servers are added/removed via CLI)
    @Published var availableServers: [ServerConfig] = []

    /// Safe accessor that returns the active project or a default empty project.
    /// Use this for UI bindings where you need a non-optional value.
    /// Always check `needsProjectCreation` before using this in contexts that require a real project.
    var safeActiveProject: ProjectConfiguration {
        activeProject ?? ProjectConfiguration.empty(id: "none", name: "No Project")
    }
    
    /// Thread-safe accessor for reading project configuration from any context.
    /// Returns nil if no valid project is configured.
    nonisolated var currentProject: ProjectConfiguration? {
        let project = ConfigurationManager.readCurrentProject()
        return project.id == "default" && project.name == "Default" ? nil : project
    }
    
    // MARK: - Slug and Name Helpers
    
    /// Generate a slug from a project name (used as filename/id)
    func slugFromName(_ name: String) -> String {
        name.lowercased()
            .trimmingCharacters(in: .whitespaces)
            .replacingOccurrences(of: " ", with: "-")
            .replacingOccurrences(of: "[^a-z0-9-]", with: "", options: .regularExpression)
            .replacingOccurrences(of: "--+", with: "-", options: .regularExpression)
            .trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    }
    
    /// Check if a project ID is available
    func isIdAvailable(_ id: String, excludingId: String? = nil) -> Bool {
        guard !id.isEmpty else { return false }
        let existingIds = availableProjectIds().filter { $0 != excludingId }
        return !existingIds.contains(id)
    }
    
    /// Check if any projects exist
    func hasProjects() -> Bool {
        !availableProjectIds().isEmpty
    }
    
    /// Check if a project name is available (case-insensitive slug comparison)
    func isNameAvailable(_ name: String, excludingId: String? = nil) -> Bool {
        let newSlug = slugFromName(name)
        guard !newSlug.isEmpty else { return false }
        
        let existingIds = availableProjectIds().filter { $0 != excludingId }
        return !existingIds.contains(newSlug)
    }
    
    private let fileManager = FileManager.default
    private let jsonEncoder: JSONEncoder
    private let jsonDecoder: JSONDecoder
    
    // File watching for external changes (CLI, text editor, etc.)
    private var projectFileSource: DispatchSourceFileSystemObject?
    private var projectFileDescriptor: Int32 = -1

    // Directory watching for projects and servers lists
    private var projectsDirectorySource: DispatchSourceFileSystemObject?
    private var projectsDirectoryDescriptor: Int32 = -1
    private var serversDirectorySource: DispatchSourceFileSystemObject?
    private var serversDirectoryDescriptor: Int32 = -1

    // Debounce to avoid rapid-fire reloads
    private var lastReloadTime: Date = .distantPast
    private let reloadDebounceInterval: TimeInterval = 0.5
    
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
        activeProject = nil

        ensureDirectoriesExist()
        load()
        refreshAvailableProjects()
        refreshAvailableServers()
        startWatchingActiveProject()
        startWatchingDirectories()
    }
    
    deinit {
        projectFileSource?.cancel()
        projectsDirectorySource?.cancel()
        serversDirectorySource?.cancel()
        if projectFileDescriptor >= 0 { close(projectFileDescriptor) }
        if projectsDirectoryDescriptor >= 0 { close(projectsDirectoryDescriptor) }
        if serversDirectoryDescriptor >= 0 { close(serversDirectoryDescriptor) }
    }
    
    // MARK: - File Watching
    
    /// Starts watching the active project's JSON file for external changes.
    /// Called on init and when switching projects.
    private func startWatchingActiveProject() {
        stopWatchingProject()
        
        guard let projectId = activeProject?.id else { return }
        let projectPath = projectsDirectory.appendingPathComponent("\(projectId).json")
        
        projectFileDescriptor = open(projectPath.path, O_EVTONLY)
        guard projectFileDescriptor >= 0 else {
            print("[ConfigurationManager] Failed to open file for watching: \(projectPath.path)")
            return
        }
        
        projectFileSource = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: projectFileDescriptor,
            eventMask: [.write, .extend, .rename],
            queue: .main
        )
        
        projectFileSource?.setEventHandler { [weak self] in
            self?.handleProjectFileChange()
        }
        
        projectFileSource?.setCancelHandler { [weak self] in
            if let fd = self?.projectFileDescriptor, fd >= 0 {
                close(fd)
            }
            self?.projectFileDescriptor = -1
        }
        
        projectFileSource?.resume()
        print("[ConfigurationManager] Watching project file: \(projectPath.lastPathComponent)")
    }
    
    /// Stops watching the current project file.
    private func stopWatchingProject() {
        projectFileSource?.cancel()
        projectFileSource = nil
    }
    
    /// Handles external changes to the active project file.
    /// Debounced to avoid rapid-fire reloads from multiple filesystem events.
    private func handleProjectFileChange() {
        let now = Date()
        guard now.timeIntervalSince(lastReloadTime) > reloadDebounceInterval else { return }
        lastReloadTime = now
        
        guard let currentId = activeProject?.id else { return }
        
        if let freshProject = loadProject(id: currentId) {
            activeProject = freshProject
            print("[ConfigurationManager] Reloaded project from disk: \(currentId)")
            NotificationCenter.default.post(name: .projectDidChange, object: nil)
        }
    }

    // MARK: - Directory Watching (Projects & Servers)

    private func startWatchingDirectories() {
        startWatchingDirectory(
            path: projectsDirectory.path,
            source: &projectsDirectorySource,
            descriptor: &projectsDirectoryDescriptor,
            onChange: { [weak self] in self?.refreshAvailableProjects() }
        )
        startWatchingDirectory(
            path: serversDirectory.path,
            source: &serversDirectorySource,
            descriptor: &serversDirectoryDescriptor,
            onChange: { [weak self] in self?.refreshAvailableServers() }
        )
    }

    private func startWatchingDirectory(
        path: String,
        source: inout DispatchSourceFileSystemObject?,
        descriptor: inout Int32,
        onChange: @escaping () -> Void
    ) {
        source?.cancel()
        source = nil

        descriptor = open(path, O_EVTONLY)
        guard descriptor >= 0 else { return }

        source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: descriptor,
            eventMask: [.write],
            queue: .main
        )

        source?.setEventHandler { onChange() }

        let fd = descriptor
        source?.setCancelHandler {
            if fd >= 0 { close(fd) }
        }

        source?.resume()
    }

    private func refreshAvailableProjects() {
        availableProjects = availableProjectIds().compactMap { loadProject(id: $0) }
    }

    private func refreshAvailableServers() {
        availableServers = availableServerIds().compactMap { loadServer(id: $0) }
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
    
    // MARK: - Project Type Sync
    
    private var projectTypesDirectory: URL {
        configDirectory.appendingPathComponent("project-types")
    }
    
    /// Syncs bundled project types to Application Support.
    /// Called on app launch to ensure CLI has access to project type definitions.
    /// Bundled types are always overwritten (they are the canonical "core" versions).
    func syncBundledProjectTypes() {
        do {
            // Ensure project-types directory exists
            try fileManager.createDirectory(at: projectTypesDirectory, withIntermediateDirectories: true)
            
            // Find bundled project types
            guard let bundledTypeURLs = Bundle.main.urls(forResourcesWithExtension: "json", subdirectory: "project-types") else {
                print("[ConfigurationManager] No bundled project types found")
                return
            }
            
            // Copy each bundled type to Application Support (overwrite existing)
            for bundledURL in bundledTypeURLs {
                let filename = bundledURL.lastPathComponent
                let destinationURL = projectTypesDirectory.appendingPathComponent(filename)
                
                // Remove existing file if present
                if fileManager.fileExists(atPath: destinationURL.path) {
                    try fileManager.removeItem(at: destinationURL)
                }
                
                // Copy bundled file
                try fileManager.copyItem(at: bundledURL, to: destinationURL)
            }
            
            print("[ConfigurationManager] Synced \(bundledTypeURLs.count) project type(s) to Application Support")
        } catch {
            print("[ConfigurationManager] Failed to sync project types: \(error)")
        }
    }

    // MARK: - Documentation Sync

    private var docsDirectory: URL {
        configDirectory.appendingPathComponent("docs")
    }

    /// Syncs bundled CLI documentation to Application Support.
    /// Called on app launch to ensure CLI has access to CLI.md.
    func syncDocumentation() {
        do {
            try fileManager.createDirectory(at: docsDirectory, withIntermediateDirectories: true)

            guard let bundledDocsURL = Bundle.main.url(forResource: "CLI", withExtension: "md", subdirectory: "docs") else {
                print("[ConfigurationManager] No bundled CLI.md found")
                return
            }

            let destinationURL = docsDirectory.appendingPathComponent("CLI.md")

            if fileManager.fileExists(atPath: destinationURL.path) {
                try fileManager.removeItem(at: destinationURL)
            }

            try fileManager.copyItem(at: bundledDocsURL, to: destinationURL)
            print("[ConfigurationManager] Synced CLI.md to Application Support")
        } catch {
            print("[ConfigurationManager] Failed to sync documentation: \(error)")
        }
    }

    // MARK: - Load Configuration
    
    private func load() {
        if fileManager.fileExists(atPath: appConfigPath.path) {
            loadFromJSON()
        } else {
            appConfig = AppConfiguration()
            saveAppConfig()
            checkProjectState()
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
            needsProjectCreation = false
        } else {
            print("[ConfigurationManager] Active project '\(appConfig.activeProjectId)' not found")
            activeProject = nil
            checkProjectState()
        }
    }
    
    /// Check if user needs to create a project (no projects exist or active project invalid)
    private func checkProjectState() {
        if !hasProjects() {
            needsProjectCreation = true
        } else if activeProject == nil {
            // Projects exist but active one is invalid - pick the first available
            if let firstId = availableProjectIds().first, let firstProject = loadProject(id: firstId) {
                activeProject = firstProject
                appConfig.activeProjectId = firstId
                saveAppConfig()
                needsProjectCreation = false
            } else {
                needsProjectCreation = true
            }
        } else {
            needsProjectCreation = false
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
            refreshAvailableProjects()
        } catch {
            print("[ConfigurationManager] Failed to save project '\(project.id)': \(error)")
        }
    }
    
    func saveActiveProject() {
        guard let project = activeProject else { return }
        saveProject(project)
    }
    
    /// Updates the active project with a mutation closure, safely merging with disk.
    /// This prevents overwriting external changes (from CLI, text editor, etc.) by:
    /// 1. Reading fresh project data from disk
    /// 2. Applying the mutation to the fresh data
    /// 3. Saving the result and updating in-memory state
    ///
    /// Use this instead of directly modifying `activeProject` and calling `saveActiveProject()`.
    func updateActiveProject(_ mutation: (inout ProjectConfiguration) -> Void) {
        guard let projectId = activeProject?.id else { return }
        
        // Read fresh from disk to avoid overwriting external changes
        guard var freshProject = loadProject(id: projectId) else {
            print("[ConfigurationManager] Failed to load project for update: \(projectId)")
            return
        }
        
        // Apply the mutation to fresh data
        mutation(&freshProject)
        
        // Save and update in-memory state
        saveProject(freshProject)
        activeProject = freshProject
    }
    
    // MARK: - Load Project
    
    func loadProject(id: String) -> ProjectConfiguration? {
        let projectPath = projectsDirectory.appendingPathComponent("\(id).json")
        guard fileManager.fileExists(atPath: projectPath.path) else {
            return nil
        }
        
        do {
            let data = try Data(contentsOf: projectPath)
            var project = try jsonDecoder.decode(ProjectConfiguration.self, from: data)
            
            // Migration: Generate default table groupings if empty
            if project.tableGroupings.isEmpty {
                project = migrateTableGroupings(project)
                saveProject(project)
            }
            
            return project
        } catch {
            print("[ConfigurationManager] Failed to load project '\(id)': \(error)")
            return nil
        }
    }
    
    /// Generates default table groupings for projects that don't have them.
    /// Uses the project type's schema definition to resolve groupings.
    private func migrateTableGroupings(_ project: ProjectConfiguration) -> ProjectConfiguration {
        var updated = project
        let groupings = SchemaResolver.resolveDefaultGroupings(for: project)
        
        if !groupings.isEmpty {
            updated.tableGroupings = groupings
            print("[ConfigurationManager] Generated \(groupings.count) table groupings for project '\(project.id)'")
        }
        
        return updated
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
        
        startWatchingActiveProject()
        
        NotificationCenter.default.post(name: .projectDidChange, object: nil)
    }
    
    /// Creates a new project with the given ID, name, and type.
    func createProject(id: String, name: String, projectType: String) -> ProjectConfiguration {
        let project = ProjectConfiguration.empty(id: id, name: name, projectType: projectType)
        saveProject(project)
        needsProjectCreation = false
        return project
    }
    
    /// Renames a project display name. The project ID remains unchanged.
    func renameProject(_ project: ProjectConfiguration, to newName: String) -> Result<ProjectConfiguration, ProjectRenameError> {
        var updatedProject = project
        updatedProject.name = newName
        saveProject(updatedProject)
        
        // Update activeProject if it's the one being renamed
        if activeProject?.id == project.id {
            activeProject = updatedProject
        }
        
        return .success(updatedProject)
    }
    
    func deleteProject(id: String) {
        guard id != appConfig.activeProjectId else {
            print("[ConfigurationManager] Cannot delete active project")
            return
        }

        let projectPath = projectsDirectory.appendingPathComponent("\(id).json")
        try? fileManager.removeItem(at: projectPath)
        refreshAvailableProjects()
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
            refreshAvailableServers()
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
    
    func loadAllServers() -> [ServerConfig] {
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
        refreshAvailableServers()
    }

    /// Returns the server for the active project
    func serverForActiveProject() -> ServerConfig? {
        guard let project = activeProject, let serverId = project.serverId else { return nil }
        return loadServer(id: serverId)
    }
}
