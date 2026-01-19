import Foundation
import SwiftUI

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

// MARK: - Component Errors

enum ComponentError: LocalizedError {
    case projectNotFound(String)
    case componentNotFound(String)
    case duplicateId(String)

    var errorDescription: String? {
        switch self {
        case .projectNotFound(let id):
            return "Project '\(id)' not found"
        case .componentNotFound(let id):
            return "Component '\(id)' not found"
        case .duplicateId(let id):
            return "Component with ID '\(id)' already exists"
        }
    }
}

/// Singleton manager for loading and saving JSON configuration files.
/// Each project is stored as a separate JSON file named by ID (e.g., extrachill.json).
/// The project ID (slug) is derived from the project name and serves as the source of truth.
@MainActor
class ConfigurationManager: ObservableObject {
    static let shared = ConfigurationManager()
    
    /// Shared decoder configured for snake_case keys from CLI JSON
    private static var decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    /// Thread-safe static accessor for reading project configuration from any context.
    nonisolated static func readCurrentProject() -> ProjectConfiguration {
        let fileManager = FileManager.default
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase

        guard let files = try? fileManager.contentsOfDirectory(at: AppPaths.projects, includingPropertiesForKeys: nil) else {
            return ProjectConfiguration.empty(id: "default", name: "Default")
        }

        let projectFiles = files
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }

        guard let firstFile = projectFiles.first,
              let data = try? Data(contentsOf: firstFile),
              let project = try? decoder.decode(ProjectConfiguration.self, from: data) else {
            return ProjectConfiguration.empty(id: "default", name: "Default")
        }

        return project
    }

    /// Thread-safe static accessor for reading any project by ID.
    nonisolated static func readProject(id: String) -> ProjectConfiguration? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let data = try? Data(contentsOf: AppPaths.project(id: id)),
              let config = try? decoder.decode(ProjectConfiguration.self, from: data) else {
            return nil
        }
        return config
    }

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
    nonisolated static func slugFromName(_ name: String) -> String {
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
        let newSlug = Self.slugFromName(name)
        guard !newSlug.isEmpty else { return false }
        
        let existingIds = availableProjectIds().filter { $0 != excludingId }
        return !existingIds.contains(newSlug)
    }
    
    private let fileManager = FileManager.default
    private let jsonEncoder: JSONEncoder
    private var jsonDecoder: JSONDecoder
    
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
        AppPaths.homeboy
    }

    private var projectsDirectory: URL {
        AppPaths.projects
    }

    private var serversDirectory: URL {
        AppPaths.servers
    }

    
    // MARK: - Initialization
    
    private init() {
        jsonEncoder = JSONEncoder()
        jsonEncoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        jsonDecoder = JSONDecoder()
        jsonDecoder.keyDecodingStrategy = .convertFromSnakeCase

        activeProject = nil

        ensureDirectoriesExist()
        load()
        refreshAvailableServers()
        refreshAvailableComponents()
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
    /// Note: ConfigurationObserver handles detailed field detection; this just reloads data.
    private func handleProjectFileChange() {
        let now = Date()
        guard now.timeIntervalSince(lastReloadTime) > reloadDebounceInterval else { return }
        lastReloadTime = now

        guard let currentId = activeProject?.id else { return }

        if let freshProject = loadProject(id: currentId) {
            activeProject = freshProject
            print("[ConfigurationManager] Reloaded project from disk: \(currentId)")
            // ConfigurationObserver detects and publishes changes via its own file watching
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
            try fileManager.createDirectory(at: componentsDirectory, withIntermediateDirectories: true)
        } catch {
            print("[ConfigurationManager] Failed to create directories: \(error)")
        }
    }

    // MARK: - Load Configuration

    private func load() {
        // Synchronous initialization - just set up defaults
        // Actual project loading happens via loadProjectsFromCLI() called from HomeboyApp
        activeProject = nil
        // needsProjectCreation stays false until loadProjectsFromCLI() determines there are no projects
    }

    // MARK: - CLI Integration

    /// Load projects from CLI (primary data source)
    /// Called from HomeboyApp.swift on startup
    func loadProjectsFromCLI() async {
        do {
            let items = try await HomeboyCLI.shared.projectList()
            availableProjects = items.map { ProjectConfiguration.fromListItem($0) }

            // Load first project as active if none selected
            if activeProject == nil, let first = items.first {
                await loadProjectFromCLI(id: first.id)
            }

            needsProjectCreation = items.isEmpty
        } catch {
            print("[ConfigurationManager] CLI project list failed: \(error)")
            needsProjectCreation = true
        }
    }

    /// Load full project config from CLI
    func loadProjectFromCLI(id: String) async {
        do {
            let output = try await HomeboyCLI.shared.projectShow(id: id)
            guard let projectId = output.projectId, let config = output.project else {
                print("[ConfigurationManager] CLI project show returned no data")
                return
            }
            activeProject = ProjectConfiguration(projectId: projectId, config: config)
            startWatchingActiveProject()
        } catch {
            print("[ConfigurationManager] CLI project show failed: \(error)")
        }
    }
    
    /// Save project via CLI
    func saveProject(_ project: ProjectConfiguration) async throws {
        let json = try jsonEncoder.encode(project)
        guard let jsonString = String(data: json, encoding: .utf8) else {
            throw CLIBridgeError.invalidResponse("Failed to encode project to JSON")
        }
        _ = try await HomeboyCLI.shared.projectSet(id: project.id, json: jsonString)
        refreshAvailableProjects()
    }

    func saveActiveProject() async throws {
        guard let project = activeProject else { return }
        try await saveProject(project)
    }
    
    /// Updates the active project with a mutation closure, safely merging with disk.
    /// This prevents overwriting external changes (from CLI, text editor, etc.) by:
    /// 1. Reading fresh project data from CLI
    /// 2. Applying the mutation to the fresh data
    /// 3. Saving the result via CLI and updating in-memory state
    ///
    /// Use this instead of directly modifying `activeProject` and calling `saveActiveProject()`.
    func updateActiveProject(_ mutation: (inout ProjectConfiguration) -> Void) async throws {
        guard let projectId = activeProject?.id else { return }

        // Read fresh from CLI to avoid overwriting external changes
        let output = try await HomeboyCLI.shared.projectShow(id: projectId)
        guard let id = output.projectId, let config = output.project else {
            throw CLIBridgeError.invalidResponse("Project not found: \(projectId)")
        }
        var freshProject = ProjectConfiguration(projectId: id, config: config)

        // Apply the mutation to fresh data
        mutation(&freshProject)

        // Save via CLI and update in-memory state
        try await saveProject(freshProject)
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
            let project = try jsonDecoder.decode(ProjectConfiguration.self, from: data)
            return project
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
    
    func switchToProject(id: String) async {
        let oldId = activeProject?.id

        // Publish willSwitch via ConfigurationObserver
        ConfigurationObserver.shared.publish(.projectWillSwitch(from: oldId, to: id))

        await loadProjectFromCLI(id: id)

        // Publish didSwitch via ConfigurationObserver
        ConfigurationObserver.shared.publish(.projectDidSwitch(projectId: id))
    }
    
    /// Creates a new project via CLI.
    func createProject(name: String, domain: String) async throws -> ProjectConfiguration {
        let output = try await HomeboyCLI.shared.projectCreate(name: name, domain: domain)
        guard let projectId = output.projectId, let config = output.project else {
            throw CLIBridgeError.invalidResponse("Project creation failed")
        }
        let project = ProjectConfiguration(projectId: projectId, config: config)
        needsProjectCreation = false
        refreshAvailableProjects()
        return project
    }
    
    /// Renames a project display name via CLI. The project ID remains unchanged.
    func renameProject(_ project: ProjectConfiguration, to newName: String) async throws -> ProjectConfiguration {
        var updatedProject = project
        updatedProject.name = newName
        try await saveProject(updatedProject)

        // Update activeProject if it's the one being renamed
        if activeProject?.id == project.id {
            activeProject = updatedProject
        }

        return updatedProject
    }
    
    /// Delete project via CLI
    func deleteProject(id: String) async throws {
        guard id != activeProject?.id else {
            throw CLIBridgeError.invalidResponse("Cannot delete active project")
        }

        try await HomeboyCLI.shared.projectDelete(id: id)
        refreshAvailableProjects()
    }

    // MARK: - Component Management

    /// Get a specific component from a project
    nonisolated static func getComponent(projectId: String, componentId: String) -> ComponentConfiguration? {
        guard let project = readProject(id: projectId) else { return nil }
        guard project.componentIds.contains(componentId) else { return nil }
        return readComponent(id: componentId)
    }

    /// Add a component to a project (thread-safe)
    nonisolated static func addComponent(projectId: String, _ component: ComponentConfiguration) throws {
        guard var project = readProject(id: projectId) else {
            throw ComponentError.projectNotFound(projectId)
        }

        guard !project.componentIds.contains(component.id) else {
            throw ComponentError.duplicateId(component.id)
        }

        // Save component file
        try saveComponentConfig(component)

        // Update project componentIds
        project.componentIds.append(component.id)
        try saveProjectConfig(project)
    }

    /// Update an existing component (thread-safe)
    nonisolated static func updateComponent(projectId: String, _ component: ComponentConfiguration) throws {
        guard let project = readProject(id: projectId) else {
            throw ComponentError.projectNotFound(projectId)
        }

        guard project.componentIds.contains(component.id) else {
            throw ComponentError.componentNotFound(component.id)
        }

        try saveComponentConfig(component)
    }

    /// Remove a component from a project (thread-safe)
    nonisolated static func removeComponent(projectId: String, componentId: String) throws {
        guard var project = readProject(id: projectId) else {
            throw ComponentError.projectNotFound(projectId)
        }

        guard let index = project.componentIds.firstIndex(of: componentId) else {
            throw ComponentError.componentNotFound(componentId)
        }

        project.componentIds.remove(at: index)
        try saveProjectConfig(project)
    }

    /// Thread-safe component file save
    nonisolated private static func saveComponentConfig(_ component: ComponentConfiguration) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(component)
        try data.write(to: AppPaths.component(id: component.id))
    }

    /// Thread-safe project save for CLI/background use
    nonisolated private static func saveProjectConfig(_ project: ProjectConfiguration) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(project)
        try data.write(to: AppPaths.project(id: project.id))
    }

    /// Thread-safe static method to update and save a project configuration.
    /// Used by CLI commands to modify project settings (e.g., pinning logs).
    nonisolated static func updateProject(_ project: ProjectConfiguration) throws {
        try saveProjectConfig(project)
    }

    // MARK: - Server Management
    
    /// Thread-safe static accessor for reading a server configuration from any context.
    nonisolated static func readServer(id: String) -> ServerConfig? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let data = try? Data(contentsOf: AppPaths.server(id: id)),
              let server = try? decoder.decode(ServerConfig.self, from: data) else {
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
    
    /// Save server via CLI
    func saveServer(_ server: ServerConfig) async throws {
        let json = try jsonEncoder.encode(server)
        guard let jsonString = String(data: json, encoding: .utf8) else {
            throw CLIBridgeError.invalidResponse("Failed to encode server to JSON")
        }
        _ = try await HomeboyCLI.shared.serverSet(id: server.id, json: jsonString)
        refreshAvailableServers()
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
    
    /// Delete server via CLI (CLI handles project reference check)
    func deleteServer(id: String) async throws {
        try await HomeboyCLI.shared.serverDelete(id: id)
        refreshAvailableServers()
    }

    /// Returns the server for the active project
    func serverForActiveProject() -> ServerConfig? {
        guard let project = activeProject, let serverId = project.serverId else { return nil }
        return loadServer(id: serverId)
    }

    // MARK: - Component Management

    private var componentsDirectory: URL {
        AppPaths.components
    }

    /// Reactive list of available components (updates when components are added/removed)
    @Published var availableComponents: [ComponentConfiguration] = []

    /// Thread-safe static accessor for reading a component configuration from any context.
    nonisolated static func readComponent(id: String) -> ComponentConfiguration? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let data = try? Data(contentsOf: AppPaths.component(id: id)),
              let component = try? decoder.decode(ComponentConfiguration.self, from: data) else {
            return nil
        }
        return component
    }

    /// Save component via CLI
    func saveComponent(_ component: ComponentConfiguration) async throws {
        let json = try jsonEncoder.encode(component)
        guard let jsonString = String(data: json, encoding: .utf8) else {
            throw CLIBridgeError.invalidResponse("Failed to encode component to JSON")
        }
        _ = try await HomeboyCLI.shared.componentSet(id: component.id, json: jsonString)
        refreshAvailableComponents()
    }

    func loadComponent(id: String) -> ComponentConfiguration? {
        let componentPath = componentsDirectory.appendingPathComponent("\(id).json")
        guard fileManager.fileExists(atPath: componentPath.path) else {
            return nil
        }

        do {
            let data = try Data(contentsOf: componentPath)
            return try jsonDecoder.decode(ComponentConfiguration.self, from: data)
        } catch {
            print("[ConfigurationManager] Failed to load component '\(id)': \(error)")
            return nil
        }
    }

    func availableComponentIds() -> [String] {
        do {
            let files = try fileManager.contentsOfDirectory(at: componentsDirectory, includingPropertiesForKeys: nil)
            return files
                .filter { $0.pathExtension == "json" }
                .map { $0.deletingPathExtension().lastPathComponent }
        } catch {
            print("[ConfigurationManager] Failed to list components: \(error)")
            return []
        }
    }

    func loadAllComponents() -> [ComponentConfiguration] {
        availableComponentIds().compactMap { loadComponent(id: $0) }
    }

    /// Load components for a project by resolving componentIds to ComponentConfiguration objects.
    func loadComponentsForProject(_ project: ProjectConfiguration) -> [ComponentConfiguration] {
        project.componentIds.compactMap { loadComponent(id: $0) }
    }

    /// Thread-safe version for loading components for a project
    nonisolated static func loadComponentsForProject(_ project: ProjectConfiguration) -> [ComponentConfiguration] {
        project.componentIds.compactMap { readComponent(id: $0) }
    }

    /// Delete component via CLI (CLI handles project reference check)
    func deleteComponent(id: String) async throws {
        try await HomeboyCLI.shared.componentDelete(id: id)
        refreshAvailableComponents()
    }

    private func refreshAvailableComponents() {
        availableComponents = availableComponentIds().compactMap { loadComponent(id: $0) }
    }
}
