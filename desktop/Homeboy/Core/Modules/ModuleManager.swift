import Combine
import Foundation
import SwiftUI

/// Module loading state derived from CLI response
enum ModuleState: Equatable {
    case ready
    case needsSetup
    case installing
    case missingRequirements([String])
    case error(String)
}

/// Represents a loaded module with its manifest and state
/// Manifest is loaded from CLI-reported path for UI rendering
struct LoadedModule: Identifiable {
    let manifest: ModuleManifest
    var state: ModuleState
    let cliEntry: CLIModuleEntry

    var id: String { manifest.id }
    var name: String { manifest.name }
    var icon: String { manifest.icon }
    var modulePath: String { cliEntry.path }
    var isLinked: Bool { cliEntry.linked }

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

    var isDisabled: Bool {
        if case .missingRequirements = state { return true }
        return false
    }

    var missingComponents: [String] {
        if case .missingRequirements(let components) = state { return components }
        return []
    }
}

/// Singleton manager for module operations via CLI delegation
/// CLI is the single source of truth for module discovery and installation.
/// Desktop reads manifests from CLI-reported paths for UI rendering.
@MainActor
class ModuleManager: ObservableObject, ConfigurationObserving {
    static let shared = ModuleManager()

    var cancellables = Set<AnyCancellable>()

    @Published var modules: [LoadedModule] = []
    @Published var isLoading = false
    @Published var error: (any DisplayableError)?

    private let fileManager = FileManager.default
    private var jsonDecoder: JSONDecoder
    private let jsonEncoder: JSONEncoder

    private init() {
        jsonDecoder = JSONDecoder()
        jsonDecoder.keyDecodingStrategy = .convertFromSnakeCase
        jsonEncoder = JSONEncoder()
        jsonEncoder.outputFormatting = [.prettyPrinted, .sortedKeys]

        Task {
            await loadModules()
        }
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            Task { await loadModules() }
        case .moduleAdded, .moduleRemoved, .moduleModified:
            Task { await loadModules() }
        default:
            break
        }
    }

    // MARK: - Module Discovery via CLI

    /// Loads modules from CLI and reads manifests for UI rendering
    func loadModules() async {
        isLoading = true
        error = nil

        do {
            let projectId = ConfigurationManager.shared.activeProject?.id
            var args = ["module", "list", "--json"]
            if let project = projectId {
                args += ["--project", project]
            }

            let response = try await CLIBridge.shared.execute(args)
            let result = try response.decodeResponse(CLIModuleListData.self)

            guard result.success, let data = result.data else {
                if let errorDetail = result.error {
                    self.error = errorDetail.toCLIError(source: "Module Manager")
                } else {
                    self.error = AppError("Failed to load modules", source: "Module Manager")
                }
                self.modules = []
                isLoading = false
                return
            }

            // Filter to executable modules only (platform modules don't have UI)
            let executableEntries = data.modules.filter { $0.runtime == "executable" }

            // Load manifests from CLI-reported paths
            var loadedModules: [LoadedModule] = []
            for entry in executableEntries {
                if let module = loadManifest(from: entry) {
                    loadedModules.append(module)
                }
            }

            modules = loadedModules.sorted { $0.name < $1.name }

        } catch {
            self.error = error.toDisplayableError(source: "Module Manager")
            modules = []
        }

        isLoading = false
    }

    /// Loads manifest from CLI-reported path and creates LoadedModule
    private func loadManifest(from entry: CLIModuleEntry) -> LoadedModule? {
        let manifestPath = URL(fileURLWithPath: entry.path).appendingPathComponent("homeboy.json")

        guard fileManager.fileExists(atPath: manifestPath.path) else {
            print("[ModuleManager] No homeboy.json at \(entry.path)")
            return nil
        }

        do {
            let data = try Data(contentsOf: manifestPath)
            var manifest = try jsonDecoder.decode(ModuleManifest.self, from: data)
            manifest.modulePath = entry.path

            let state = deriveState(from: entry)

            return LoadedModule(
                manifest: manifest,
                state: state,
                cliEntry: entry
            )
        } catch {
            print("[ModuleManager] Failed to load manifest at \(entry.path): \(error)")
            return nil
        }
    }

    /// Derives ModuleState from CLI entry
    private func deriveState(from entry: CLIModuleEntry) -> ModuleState {
        if !entry.compatible {
            return .missingRequirements(["Incompatible with current project"])
        }
        if !entry.ready {
            return .needsSetup
        }
        return .ready
    }


    // MARK: - Module Installation via CLI

    /// Installs a module from a Git URL
    func installModule(from url: String) async throws {
        let args = ["module", "install", url]
        let response = try await CLIBridge.shared.execute(args, timeout: 300)

        guard response.success else {
            throw ModuleError.installFailed
        }

        await loadModules()
    }

    /// Links a local module directory (uses install which auto-symlinks local paths)
    func linkModule(path: String) async throws {
        let args = ["module", "install", path]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ModuleError.installFailed
        }

        await loadModules()
    }

    /// Unlinks a linked module (uses uninstall which handles symlinks)
    func unlinkModule(moduleId: String) async throws {
        let args = ["module", "uninstall", moduleId, "--force"]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ModuleError.moduleNotFound
        }

        await loadModules()
    }

    /// Uninstalls a module via CLI
    func uninstallModule(moduleId: String) async throws {
        let args = ["module", "uninstall", moduleId, "--force"]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ModuleError.moduleNotFound
        }

        await loadModules()
    }

    /// Sets up a module via CLI
    func setupModule(moduleId: String) async throws {
        let args = ["module", "setup", moduleId]
        let response = try await CLIBridge.shared.execute(args, timeout: 600)

        guard response.success else {
            throw ModuleError.setupFailed(response.errorOutput)
        }

        await loadModules()
    }

    // MARK: - Module Execution via CLI

    /// Runs a module via CLI and streams output
    func runModule(
        moduleId: String,
        inputs: [String: String],
        projectId: String?,
        onOutput: @escaping (String) -> Void
    ) async {
        var args = ["module", "run", moduleId]
        if let project = projectId {
            args += ["--project", project]
        }
        for (key, value) in inputs {
            args += ["--input", "\(key)=\(value)"]
        }

        let stream = await CLIBridge.shared.executeStreaming(args)
        for await line in stream {
            onOutput(line)
        }
    }

    /// Updates a module's state
    func updateModuleState(moduleId: String, state: ModuleState) {
        guard let index = modules.firstIndex(where: { $0.id == moduleId }) else { return }
        modules[index].state = state
    }

    // MARK: - Module Lookup

    func module(withId id: String) -> LoadedModule? {
        modules.first { $0.id == id }
    }
}

// MARK: - Errors

enum ModuleError: LocalizedError {
    case missingManifest
    case invalidManifest
    case moduleNotFound
    case installFailed
    case setupFailed(String)
    case cliNotInstalled

    var errorDescription: String? {
        switch self {
        case .missingManifest:
            return "No homeboy.json found in the selected directory"
        case .invalidManifest:
            return "Invalid homeboy.json format"
        case .moduleNotFound:
            return "Module not found"
        case .installFailed:
            return "Failed to install module"
        case .setupFailed(let reason):
            return "Setup failed: \(reason)"
        case .cliNotInstalled:
            return "Homeboy CLI is not installed"
        }
    }
}
