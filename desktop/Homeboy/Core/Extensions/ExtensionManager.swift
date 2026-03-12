import Combine
import Foundation
import SwiftUI

/// Extension loading state derived from CLI response
enum ExtensionState: Equatable {
    case ready
    case needsSetup
    case installing
    case missingRequirements([String])
    case error(String)
}

/// Represents a loaded extension with its manifest and state
/// Manifest is loaded from CLI-reported path for UI rendering
struct LoadedExtension: Identifiable {
    let manifest: ExtensionManifest
    var state: ExtensionState
    let cliEntry: CLIExtensionEntry

    var id: String { manifest.id }
    var name: String { manifest.name }
    var icon: String { manifest.icon }
    var extensionPath: String { cliEntry.path }
    var isLinked: Bool { cliEntry.linked }

    var venvPath: String {
        "\(extensionPath)/venv"
    }

    var venvPythonPath: String {
        "\(venvPath)/bin/python3"
    }

    var entrypointPath: String {
        guard let entrypoint = manifest.runtime?.entrypoint else { return "" }
        return "\(extensionPath)/\(entrypoint)"
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

/// Singleton manager for extension operations via CLI delegation
/// CLI is the single source of truth for extension discovery and installation.
/// Desktop reads manifests from CLI-reported paths for UI rendering.
@MainActor
class ExtensionManager: ObservableObject, ConfigurationObserving {
    static let shared = ExtensionManager()

    var cancellables = Set<AnyCancellable>()

    @Published var extensions: [LoadedExtension] = []
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
            await loadExtensions()
        }
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            Task { await loadExtensions() }
        case .extensionAdded, .extensionRemoved, .extensionModified:
            Task { await loadExtensions() }
        default:
            break
        }
    }

    // MARK: - Extension Discovery via CLI

    /// Loads extensions from CLI and reads manifests for UI rendering
    func loadExtensions() async {
        isLoading = true
        error = nil

        do {
            let projectId = ConfigurationManager.shared.activeProject?.id
            var args = ["extension", "list", "--json"]
            if let project = projectId {
                args += ["--project", project]
            }

            let response = try await CLIBridge.shared.execute(args)
            let result = try response.decodeResponse(CLIExtensionListData.self)

            guard result.success, let data = result.data else {
                if let errorDetail = result.error {
                    self.error = errorDetail.toCLIError(source: "Extension Manager")
                } else {
                    self.error = AppError("Failed to load extensions", source: "Extension Manager")
                }
                self.extensions = []
                isLoading = false
                return
            }

            // Include executable extensions and platform extensions that have actions
            let visibleEntries = data.extensions.filter {
                $0.runtime == "executable" || !($0.actions ?? []).isEmpty
            }

            // Load manifests from CLI-reported paths
            var loadedExtensions: [LoadedExtension] = []
            for entry in visibleEntries {
                if let extension = loadManifest(from: entry) {
                    loadedExtensions.append(extension)
                }
            }

            extensions = loadedExtensions.sorted { $0.name < $1.name }

        } catch {
            self.error = error.toDisplayableError(source: "Extension Manager")
            extensions = []
        }

        isLoading = false
    }

    /// Loads manifest from CLI-reported path and creates LoadedExtension
    private func loadManifest(from entry: CLIExtensionEntry) -> LoadedExtension? {
        let manifestPath = URL(fileURLWithPath: entry.path).appendingPathComponent("\(entry.id).json")

        guard fileManager.fileExists(atPath: manifestPath.path) else {
            print("[ExtensionManager] No \(entry.id).json at \(entry.path)")
            return nil
        }

        do {
            let data = try Data(contentsOf: manifestPath)
            var manifest = try jsonDecoder.decode(ExtensionManifest.self, from: data)
            manifest.extensionPath = entry.path

            let state = deriveState(from: entry)

            return LoadedExtension(
                manifest: manifest,
                state: state,
                cliEntry: entry
            )
        } catch {
            print("[ExtensionManager] Failed to load manifest at \(entry.path): \(error)")
            return nil
        }
    }

    /// Derives ExtensionState from CLI entry
    private func deriveState(from entry: CLIExtensionEntry) -> ExtensionState {
        if !entry.compatible {
            return .missingRequirements(["Incompatible with current project"])
        }
        if !entry.ready {
            return .needsSetup
        }
        return .ready
    }


    // MARK: - Extension Installation via CLI

    /// Installs a extension from a Git URL
    func installExtension(from url: String) async throws {
        let args = ["extension", "install", url]
        let response = try await CLIBridge.shared.execute(args, timeout: 300)

        guard response.success else {
            throw ExtensionError.installFailed
        }

        await loadExtensions()
    }

    /// Links a local extension directory (uses install which auto-symlinks local paths)
    func linkExtension(path: String) async throws {
        let args = ["extension", "install", path]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ExtensionError.installFailed
        }

        await loadExtensions()
    }

    /// Unlinks a linked extension (uses uninstall which handles symlinks)
    func unlinkExtension(extensionId: String) async throws {
        let args = ["extension", "uninstall", extensionId, "--force"]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ExtensionError.extensionNotFound
        }

        await loadExtensions()
    }

    /// Uninstalls a extension via CLI
    func uninstallExtension(extensionId: String) async throws {
        let args = ["extension", "uninstall", extensionId, "--force"]
        let response = try await CLIBridge.shared.execute(args)

        guard response.success else {
            throw ExtensionError.extensionNotFound
        }

        await loadExtensions()
    }

    /// Sets up a extension via CLI
    func setupExtension(extensionId: String) async throws {
        let args = ["extension", "setup", extensionId]
        let response = try await CLIBridge.shared.execute(args, timeout: 600)

        guard response.success else {
            throw ExtensionError.setupFailed(response.errorOutput)
        }

        await loadExtensions()
    }

    // MARK: - Extension Execution via CLI

    /// Runs a extension via CLI and streams output
    func runExtension(
        extensionId: String,
        inputs: [String: String],
        projectId: String?,
        onOutput: @escaping (String) -> Void
    ) async {
        var args = ["extension", "run", extensionId]
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

    /// Updates a extension's state
    func updateExtensionState(extensionId: String, state: ExtensionState) {
        guard let index = extensions.firstIndex(where: { $0.id == extensionId }) else { return }
        extensions[index].state = state
    }

    // MARK: - Extension Lookup

    func extension(withId id: String) -> LoadedExtension? {
        extensions.first { $0.id == id }
    }
}

// MARK: - Errors

enum ExtensionError: LocalizedError {
    case missingManifest
    case invalidManifest
    case extensionNotFound
    case installFailed
    case setupFailed(String)
    case cliNotInstalled

    var errorDescription: String? {
        switch self {
        case .missingManifest:
            return "No extension manifest found in the selected directory"
        case .invalidManifest:
            return "Invalid extension manifest format"
        case .extensionNotFound:
            return "Extension not found"
        case .installFailed:
            return "Failed to install extension"
        case .setupFailed(let reason):
            return "Setup failed: \(reason)"
        case .cliNotInstalled:
            return "Homeboy CLI is not installed"
        }
    }
}
