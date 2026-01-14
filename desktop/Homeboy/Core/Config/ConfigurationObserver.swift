import Combine
import Foundation

/// Centralized file system observer for all configuration changes.
/// Single source of truth for configuration change events in the app.
@MainActor
final class ConfigurationObserver: ObservableObject {
    static let shared = ConfigurationObserver()

    /// Published change stream for Combine subscribers
    @Published private(set) var lastChange: ConfigurationChangeType?

    // File system watchers keyed by watch identifier
    private var watchers: [String: DispatchSourceFileSystemObject] = [:]
    private var descriptors: [String: Int32] = [:]

    // Debouncing
    private var debounceWorkItems: [String: DispatchWorkItem] = [:]
    private let debounceInterval: TimeInterval = 0.3

    // Snapshots for change detection (file path -> encoded data)
    private var projectSnapshots: [String: Data] = [:]
    private var serverSnapshots: [String: Data] = [:]
    private var moduleManifestSnapshots: [String: Data] = [:]


    private let fileManager = FileManager.default
    private let jsonDecoder = JSONDecoder()
    private let jsonEncoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    private init() {
        setupWatchers()
        refreshAllSnapshots()
    }

    // Note: No deinit needed - this is a singleton that lives for app lifetime

    // MARK: - Public API

    /// Manually publish a change (called by ConfigurationManager during project switch)
    func publish(_ change: ConfigurationChangeType) {
        lastChange = change
    }

    // MARK: - File Watching Setup

    private func setupWatchers() {
        // Watch directories for add/remove/modify
        watchDirectory(AppPaths.projects, key: "projects")
        watchDirectory(AppPaths.servers, key: "servers")
        watchDirectory(AppPaths.modules, key: "modules")
        watchDirectory(AppPaths.projectTypes, key: "projectTypes")
    }

    private func watchFile(_ url: URL, key: String) {
        guard fileManager.fileExists(atPath: url.path) else { return }

        let fd = open(url.path, O_EVTONLY)
        guard fd >= 0 else { return }

        let source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd,
            eventMask: [.write, .extend, .rename],
            queue: .main
        )

        source.setEventHandler { [weak self] in
            self?.handleFileSystemEvent(key: key)
        }

        source.setCancelHandler {
            close(fd)
        }

        source.resume()

        watchers[key] = source
        descriptors[key] = fd
    }

    private func watchDirectory(_ url: URL, key: String) {
        // Create directory if it doesn't exist
        if !fileManager.fileExists(atPath: url.path) {
            try? fileManager.createDirectory(at: url, withIntermediateDirectories: true)
        }

        let fd = open(url.path, O_EVTONLY)
        guard fd >= 0 else { return }

        let source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd,
            eventMask: [.write],
            queue: .main
        )

        source.setEventHandler { [weak self] in
            self?.handleFileSystemEvent(key: key)
        }

        source.setCancelHandler {
            close(fd)
        }

        source.resume()

        watchers[key] = source
        descriptors[key] = fd
    }

    private func stopAllWatchers() {
        for (_, source) in watchers {
            source.cancel()
        }
        watchers.removeAll()
        descriptors.removeAll()
    }

    // MARK: - Event Handling

    private func handleFileSystemEvent(key: String) {
        debounceWorkItems[key]?.cancel()

        let workItem = DispatchWorkItem { [weak self] in
            self?.detectAndPublishChanges(source: key)
        }

        debounceWorkItems[key] = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + debounceInterval, execute: workItem)
    }

    private func detectAndPublishChanges(source: String) {
        switch source {
        case "projects":
            detectProjectChanges()
        case "servers":
            detectServerChanges()
        case "modules":
            detectModuleChanges()
        case "projectTypes":
            detectProjectTypeChanges()
        default:
            break
        }
    }

    // MARK: - Snapshot Management

    private func refreshAllSnapshots() {
        projectSnapshots = loadAllProjectData()
        serverSnapshots = loadAllServerData()
        moduleManifestSnapshots = loadAllModuleManifestData()
    }


    private func loadAllProjectData() -> [String: Data] {
        loadAllJsonData(from: AppPaths.projects)
    }

    private func loadAllServerData() -> [String: Data] {
        loadAllJsonData(from: AppPaths.servers)
    }

    private func loadAllModuleManifestData() -> [String: Data] {
        var result: [String: Data] = [:]
        guard let moduleIds = try? fileManager.contentsOfDirectory(atPath: AppPaths.modules.path) else {
            return result
        }
        for moduleId in moduleIds {
            let manifestPath = AppPaths.module(id: moduleId).appendingPathComponent("homeboy.json")
            if let data = try? Data(contentsOf: manifestPath) {
                result[moduleId] = data
            }
        }
        return result
    }


    private func loadAllJsonData(from directory: URL) -> [String: Data] {
        var result: [String: Data] = [:]
        guard let files = try? fileManager.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else {
            return result
        }
        for file in files where file.pathExtension == "json" {
            let id = file.deletingPathExtension().lastPathComponent
            if let data = try? Data(contentsOf: file) {
                result[id] = data
            }
        }
        return result
    }

    // MARK: - Change Detection


    private func detectProjectChanges() {
        let currentProjects = loadAllProjectData()
        let currentIds = Set(currentProjects.keys)
        let previousIds = Set(projectSnapshots.keys)

        // Check for modifications to existing projects
        for projectId in currentIds.intersection(previousIds) {
            guard let newData = currentProjects[projectId],
                  let oldData = projectSnapshots[projectId],
                  newData != oldData else { continue }

            let fields = detectModifiedFields(oldData: oldData, newData: newData)
            if !fields.isEmpty {
                publish(.projectModified(projectId: projectId, fields: fields))
            }
        }

        // Note: Added/removed projects are handled by ConfigurationManager's directory watching
        // which updates availableProjects. We only track modifications here.

        projectSnapshots = currentProjects
    }

    private func detectServerChanges() {
        let currentServers = loadAllServerData()
        let currentIds = Set(currentServers.keys)
        let previousIds = Set(serverSnapshots.keys)

        // Added servers
        for serverId in currentIds.subtracting(previousIds) {
            publish(.serverAdded(serverId: serverId))
        }

        // Removed servers
        for serverId in previousIds.subtracting(currentIds) {
            publish(.serverRemoved(serverId: serverId))
        }

        // Modified servers
        for serverId in currentIds.intersection(previousIds) {
            if let newData = currentServers[serverId],
               let oldData = serverSnapshots[serverId],
               newData != oldData {
                publish(.serverModified(serverId: serverId))
            }
        }

        serverSnapshots = currentServers
    }

    private func detectModuleChanges() {
        let currentManifests = loadAllModuleManifestData()
        let currentIds = Set(currentManifests.keys)
        let previousIds = Set(moduleManifestSnapshots.keys)

        // Added modules
        for moduleId in currentIds.subtracting(previousIds) {
            publish(.moduleAdded(moduleId: moduleId))
        }

        // Removed modules
        for moduleId in previousIds.subtracting(currentIds) {
            publish(.moduleRemoved(moduleId: moduleId))
        }

        // Modified manifests
        for moduleId in currentIds.intersection(previousIds) {
            if let newData = currentManifests[moduleId],
               let oldData = moduleManifestSnapshots[moduleId],
               newData != oldData {
                publish(.moduleModified(moduleId: moduleId))
            }
        }

        moduleManifestSnapshots = currentManifests
    }

    private func detectProjectTypeChanges() {
        // Project types are synced from bundle, so we just notify when they change
        guard let files = try? fileManager.contentsOfDirectory(at: AppPaths.projectTypes, includingPropertiesForKeys: nil) else {
            return
        }
        for file in files where file.pathExtension == "json" {
            let typeId = file.deletingPathExtension().lastPathComponent
            publish(.projectTypeModified(typeId: typeId))
        }
    }

    // MARK: - Field Detection

    /// Detect which fields changed between old and new project configs
    private func detectModifiedFields(oldData: Data, newData: Data) -> Set<ProjectField> {
        guard let oldConfig = try? jsonDecoder.decode(ProjectConfiguration.self, from: oldData),
              let newConfig = try? jsonDecoder.decode(ProjectConfiguration.self, from: newData) else {
            return []
        }

        var fields = Set<ProjectField>()

        if oldConfig.serverId != newConfig.serverId {
            fields.insert(.server)
        }
        if oldConfig.basePath != newConfig.basePath {
            fields.insert(.basePath)
        }
        if !isEqual(oldConfig.database, newConfig.database) {
            fields.insert(.database)
        }
        if oldConfig.componentIds != newConfig.componentIds {
            fields.insert(.components)
        }
        if oldConfig.subTargets != newConfig.subTargets {
            fields.insert(.subTargets)
        }
        if oldConfig.remoteFiles != newConfig.remoteFiles {
            fields.insert(.remoteFiles)
        }
        if oldConfig.remoteLogs != newConfig.remoteLogs {
            fields.insert(.remoteLogs)
        }
        if !isEqual(oldConfig.api, newConfig.api) {
            fields.insert(.api)
        }
        if !isEqual(oldConfig.tools, newConfig.tools) {
            fields.insert(.tools)
        }

        return fields
    }

    /// Compare Codable values by encoding to JSON
    private func isEqual<T: Codable>(_ lhs: T, _ rhs: T) -> Bool {
        guard let lhsData = try? jsonEncoder.encode(lhs),
              let rhsData = try? jsonEncoder.encode(rhs) else {
            return false
        }
        return lhsData == rhsData
    }
}
