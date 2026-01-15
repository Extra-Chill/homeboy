import Foundation

/// Manages project type definitions from installed modules and user-defined sources.
/// Platform modules (with cli/database config) are loaded from the shared modules directory.
/// User-defined types in ~/Library/Application Support/Homeboy/project-types/ take precedence.
final class ProjectTypeManager {

    static let shared = ProjectTypeManager()

    private var cachedTypes: [String: ProjectTypeDefinition] = [:]

    private init() {
        loadTypes()
    }

    /// Directory for user-defined project types
    private var userTypesDirectory: URL {
        AppPaths.projectTypes
    }

    /// Directory for installed modules (shared with CLI)
    private var modulesDirectory: URL {
        AppPaths.modules
    }

    /// All available project types (from modules + user-defined)
    var allTypes: [ProjectTypeDefinition] {
        Array(cachedTypes.values).sorted { $0.displayName < $1.displayName }
    }

    /// The default project type ID (first available type).
    /// Returns nil if no types are loaded.
    var defaultTypeId: String? {
        allTypes.first?.id
    }

    /// Lookup a project type by ID
    func type(for id: String) -> ProjectTypeDefinition? {
        cachedTypes[id]
    }

    /// Resolve a project type ID to its definition, falling back to generic if not found
    func resolve(_ id: String) -> ProjectTypeDefinition {
        cachedTypes[id] ?? .fallbackGeneric
    }

    /// Reload types from disk
    func reload() {
        loadTypes()
    }

    private func loadTypes() {
        cachedTypes.removeAll()

        // Load platform modules from the shared modules directory
        loadModuleTypes()

        // Load user-defined types (override modules if same ID)
        loadUserTypes()
    }

    /// Loads platform modules (those with cli or database config) from the modules directory
    private func loadModuleTypes() {
        let fileManager = FileManager.default

        guard fileManager.fileExists(atPath: modulesDirectory.path),
              let contents = try? fileManager.contentsOfDirectory(at: modulesDirectory, includingPropertiesForKeys: [.isDirectoryKey]) else {
            return
        }

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        for itemURL in contents {
            var isDir: ObjCBool = false
            guard fileManager.fileExists(atPath: itemURL.path, isDirectory: &isDir), isDir.boolValue else {
                continue
            }

            let manifestPath = itemURL.appendingPathComponent("homeboy.json")
            guard let data = try? Data(contentsOf: manifestPath),
                  let type = try? decoder.decode(ProjectTypeDefinition.self, from: data) else {
                continue
            }

            // Only include modules that have platform configuration (cli or database)
            if type.hasCLI || type.database != nil {
                cachedTypes[type.id] = type
            }
        }
    }

    private func loadUserTypes() {
        let fileManager = FileManager.default

        // Ensure directory exists
        if !fileManager.fileExists(atPath: userTypesDirectory.path) {
            try? fileManager.createDirectory(at: userTypesDirectory, withIntermediateDirectories: true)
            return
        }

        loadTypesFromDirectory(userTypesDirectory)
    }

    private func loadTypesFromDirectory(_ directory: URL) {
        guard let files = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else {
            return
        }

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        for file in files where file.pathExtension == "json" {
            guard let data = try? Data(contentsOf: file),
                  let type = try? decoder.decode(ProjectTypeDefinition.self, from: data) else {
                continue
            }
            cachedTypes[type.id] = type
        }
    }
    
    /// Save a user-defined project type to disk
    func saveUserType(_ type: ProjectTypeDefinition) throws {
        let fileManager = FileManager.default
        
        // Ensure directory exists
        if !fileManager.fileExists(atPath: userTypesDirectory.path) {
            try fileManager.createDirectory(at: userTypesDirectory, withIntermediateDirectories: true)
        }
        
        let fileURL = userTypesDirectory.appendingPathComponent("\(type.id).json")
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(type)
        try data.write(to: fileURL)
        
        // Update cache
        cachedTypes[type.id] = type
    }
    
    /// Delete a user-defined project type
    func deleteUserType(id: String) throws {
        let fileURL = userTypesDirectory.appendingPathComponent("\(id).json")
        try FileManager.default.removeItem(at: fileURL)
        
        // Reload to restore bundled type if one exists with same ID
        loadTypes()
    }
}
