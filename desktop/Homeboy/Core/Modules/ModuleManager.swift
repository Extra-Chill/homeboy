import Combine
import Foundation
import SwiftUI

/// Module loading state
enum ModuleState: Equatable {
    case ready
    case needsSetup
    case installing
    case missingRequirements([String])
    case error(String)
}

/// Represents a loaded module with its manifest and state
struct LoadedModule: Identifiable {
    let manifest: ModuleManifest
    var state: ModuleState
    var settings: ModuleSettings
    
    var id: String { manifest.id }
    var name: String { manifest.name }
    var icon: String { manifest.icon }
    var modulePath: String { manifest.modulePath ?? "" }
    
    var venvPath: String {
        "\(modulePath)/venv"
    }
    
    var venvPythonPath: String {
        "\(venvPath)/bin/python3"
    }
    
    var entrypointPath: String {
        guard let entrypoint = manifest.runtime.entrypoint else { return "" }
        return "\(modulePath)/\(entrypoint)"
    }
    
    var settingsPath: String {
        "\(modulePath)/config.json"
    }
    
    var isDisabled: Bool {
        if case .missingRequirements = state { return true }
        return false
    }
    
    var missingComponents: [String] {
        if case .missingRequirements(let components) = state { return components }
        return []
    }
}

/// Singleton manager for discovering, loading, and managing modules
@MainActor
class ModuleManager: ObservableObject {
    static let shared = ModuleManager()
    
    private var cancellables = Set<AnyCancellable>()
    
    @Published var modules: [LoadedModule] = []
    @Published var isLoading = false
    
    private let fileManager = FileManager.default
    private let jsonDecoder: JSONDecoder
    private let jsonEncoder: JSONEncoder
    
    /// Base directory for modules
    /// ~/Library/Application Support/Homeboy/modules/
    var modulesDirectory: URL {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy/modules")
    }
    
    /// Shared Playwright browsers location
    /// ~/Library/Application Support/Homeboy/playwright-browsers/
    var playwrightBrowsersPath: String {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy/playwright-browsers").path
    }
    
    private init() {
        jsonDecoder = JSONDecoder()
        jsonEncoder = JSONEncoder()
        jsonEncoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        
        ensureDirectoriesExist()
        loadModules()
        setupSiteChangeObserver()
    }
    
    // MARK: - Site Switching
    
    private func setupSiteChangeObserver() {
        NotificationCenter.default.publisher(for: .projectDidChange)
            .sink { [weak self] _ in
                // Re-evaluate module requirements for new site
                self?.loadModules()
            }
            .store(in: &cancellables)
    }
    
    // MARK: - Directory Management
    
    private func ensureDirectoriesExist() {
        do {
            try fileManager.createDirectory(at: modulesDirectory, withIntermediateDirectories: true)
            try fileManager.createDirectory(
                atPath: playwrightBrowsersPath,
                withIntermediateDirectories: true,
                attributes: nil
            )
        } catch {
            print("[ModuleManager] Failed to create directories: \(error)")
        }
    }
    
    // MARK: - Module Discovery & Loading
    
    /// Scans the modules directory and loads all valid modules
    func loadModules() {
        isLoading = true
        modules = []
        
        do {
            let contents = try fileManager.contentsOfDirectory(
                at: modulesDirectory,
                includingPropertiesForKeys: [.isDirectoryKey]
            )
            
            for itemURL in contents {
                guard isDirectory(itemURL) else { continue }
                
                if let module = loadModule(at: itemURL) {
                    modules.append(module)
                }
            }
            
            // Sort by name
            modules.sort { $0.name < $1.name }
            
        } catch {
            print("[ModuleManager] Failed to scan modules directory: \(error)")
        }
        
        isLoading = false
    }
    
    /// Loads a single module from a directory
    private func loadModule(at url: URL) -> LoadedModule? {
        let manifestPath = url.appendingPathComponent("module.json")
        
        guard fileManager.fileExists(atPath: manifestPath.path) else {
            print("[ModuleManager] No module.json found at \(url.path)")
            return nil
        }
        
        do {
            let data = try Data(contentsOf: manifestPath)
            var manifest = try jsonDecoder.decode(ModuleManifest.self, from: data)
            manifest.modulePath = url.path
            
            let state = determineModuleState(manifest: manifest, modulePath: url.path)
            let settings = loadModuleSettings(modulePath: url.path)
            
            return LoadedModule(manifest: manifest, state: state, settings: settings)
            
        } catch {
            print("[ModuleManager] Failed to load module at \(url.path): \(error)")
            return nil
        }
    }
    
    /// Determines the current state of a module
    private func determineModuleState(manifest: ModuleManifest, modulePath: String) -> ModuleState {
        // Check requirements first
        if let missingComponents = checkRequirements(manifest: manifest), !missingComponents.isEmpty {
            return .missingRequirements(missingComponents)
        }
        
        // For Python modules, check entrypoint and venv
        if manifest.runtime.type == .python {
            guard let entrypoint = manifest.runtime.entrypoint else {
                return .error("Missing entrypoint in manifest")
            }
            
            let entrypointPath = "\(modulePath)/\(entrypoint)"
            guard fileManager.fileExists(atPath: entrypointPath) else {
                return .error("Missing entrypoint: \(entrypoint)")
            }
            
            let venvPythonPath = "\(modulePath)/venv/bin/python3"
            if !fileManager.fileExists(atPath: venvPythonPath) {
                return .needsSetup
            }
            
            // Check if Playwright browsers are needed but missing
            if let browsers = manifest.runtime.playwrightBrowsers, !browsers.isEmpty {
                let chromiumExists = (try? fileManager.contentsOfDirectory(atPath: playwrightBrowsersPath))?
                    .contains { $0.hasPrefix("chromium-") } ?? false
                
                if browsers.contains("chromium") && !chromiumExists {
                    return .needsSetup
                }
            }
        }
        
        return .ready
    }
    
    /// Checks if a module's requirements are satisfied (components, features, projectType)
    private func checkRequirements(manifest: ModuleManifest) -> [String]? {
        guard let requires = manifest.requires else {
            return nil
        }
        
        var missing: [String] = []
        let project = ConfigurationManager.shared.safeActiveProject
        
        // Check projectType requirement
        if let requiredProjectType = requires.projectType {
            if project.projectType != requiredProjectType {
                let typeDef = ProjectTypeManager.shared.resolve(requiredProjectType)
                missing.append("Project type: \(typeDef.displayName)")
            }
        }
        
        // Check feature requirements
        // Most features are now universal (always available):
        // - hasDatabase, hasDeployer, hasRemoteLogs, hasRemoteFileEditor: always true
        // - hasCLI: true if project type has CLI config
        if let requiredFeatures = requires.features {
            let typeDefinition = project.typeDefinition
            for feature in requiredFeatures {
                let isSatisfied: Bool
                switch feature {
                case "hasCLI":
                    isSatisfied = typeDefinition.hasCLI
                case "hasDatabase", "hasDeployer", "hasRemoteDeployment", "hasRemoteLogs", "hasRemoteFileEditor":
                    isSatisfied = true  // Universal features, always available
                default:
                    isSatisfied = false
                }
                if !isSatisfied {
                    missing.append(feature)
                }
            }
        }
        
        // Check component requirements
        if let requiredComponents = requires.components, !requiredComponents.isEmpty {
            let installedComponentIds = Set(project.components.map { $0.id })
            for component in requiredComponents {
                if !installedComponentIds.contains(component) {
                    missing.append(component)
                }
            }
        }
        
        return missing.isEmpty ? nil : missing
    }
    
    // MARK: - Module Settings
    
    /// Loads settings for a module from its config.json
    private func loadModuleSettings(modulePath: String) -> ModuleSettings {
        let settingsPath = "\(modulePath)/config.json"
        
        guard fileManager.fileExists(atPath: settingsPath),
              let data = try? Data(contentsOf: URL(fileURLWithPath: settingsPath)),
              let settings = try? jsonDecoder.decode(ModuleSettings.self, from: data) else {
            return ModuleSettings()
        }
        
        return settings
    }
    
    /// Saves settings for a module
    func saveModuleSettings(moduleId: String, settings: ModuleSettings) {
        guard let index = modules.firstIndex(where: { $0.id == moduleId }) else { return }
        
        let settingsPath = modules[index].settingsPath
        
        do {
            let data = try jsonEncoder.encode(settings)
            try data.write(to: URL(fileURLWithPath: settingsPath))
            modules[index].settings = settings
        } catch {
            print("[ModuleManager] Failed to save settings for \(moduleId): \(error)")
        }
    }
    
    /// Updates a specific setting value
    func updateSetting(moduleId: String, key: String, value: SettingValue) {
        guard let index = modules.firstIndex(where: { $0.id == moduleId }) else { return }
        
        var settings = modules[index].settings
        settings.values[key] = value
        saveModuleSettings(moduleId: moduleId, settings: settings)
    }
    
    // MARK: - Module Installation
    
    /// Installs a module from a source directory (copies to modules directory)
    func installModule(from sourcePath: URL) -> Result<LoadedModule, Error> {
        let manifestPath = sourcePath.appendingPathComponent("module.json")
        
        // Validate manifest exists
        guard fileManager.fileExists(atPath: manifestPath.path) else {
            return .failure(ModuleError.missingManifest)
        }
        
        // Load manifest to get module ID
        do {
            let data = try Data(contentsOf: manifestPath)
            let manifest = try jsonDecoder.decode(ModuleManifest.self, from: data)
            
            let destinationPath = modulesDirectory.appendingPathComponent(manifest.id)
            
            // Remove existing if present
            if fileManager.fileExists(atPath: destinationPath.path) {
                try fileManager.removeItem(at: destinationPath)
            }
            
            // Copy module directory
            try fileManager.copyItem(at: sourcePath, to: destinationPath)
            
            // Load the installed module
            if let module = loadModule(at: destinationPath) {
                modules.append(module)
                modules.sort { $0.name < $1.name }
                return .success(module)
            } else {
                return .failure(ModuleError.installFailed)
            }
            
        } catch {
            return .failure(error)
        }
    }
    
    /// Uninstalls a module by removing its directory
    func uninstallModule(moduleId: String) -> Result<Void, Error> {
        guard let index = modules.firstIndex(where: { $0.id == moduleId }) else {
            return .failure(ModuleError.moduleNotFound)
        }
        
        let modulePath = modules[index].modulePath
        
        do {
            try fileManager.removeItem(atPath: modulePath)
            modules.remove(at: index)
            return .success(())
        } catch {
            return .failure(error)
        }
    }
    
    /// Updates a module's state
    func updateModuleState(moduleId: String, state: ModuleState) {
        guard let index = modules.firstIndex(where: { $0.id == moduleId }) else { return }
        modules[index].state = state
    }
    
    // MARK: - Module Lookup
    
    /// Gets a module by ID
    func module(withId id: String) -> LoadedModule? {
        modules.first { $0.id == id }
    }
    
    // MARK: - Helpers
    
    private func isDirectory(_ url: URL) -> Bool {
        var isDir: ObjCBool = false
        return fileManager.fileExists(atPath: url.path, isDirectory: &isDir) && isDir.boolValue
    }
}

// MARK: - Errors

enum ModuleError: LocalizedError {
    case missingManifest
    case invalidManifest
    case moduleNotFound
    case installFailed
    case setupFailed(String)
    
    var errorDescription: String? {
        switch self {
        case .missingManifest:
            return "No module.json found in the selected directory"
        case .invalidManifest:
            return "Invalid module.json format"
        case .moduleNotFound:
            return "Module not found"
        case .installFailed:
            return "Failed to install module"
        case .setupFailed(let reason):
            return "Setup failed: \(reason)"
        }
    }
}
