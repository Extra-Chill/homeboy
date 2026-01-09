import Foundation

/// Manages project type definitions from built-in and user-defined sources.
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
    
    /// All available project types (built-in + user-defined)
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
        
        // Load bundled types from Resources/project-types/
        loadBundledTypes()
        
        // Load user-defined types (override bundled if same ID)
        loadUserTypes()
    }
    
    private func loadBundledTypes() {
        guard let bundleURL = Bundle.main.resourceURL?.appendingPathComponent("project-types") else {
            return
        }
        
        loadTypesFromDirectory(bundleURL)
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
