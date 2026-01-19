import AppKit
import Combine
import Foundation
import SwiftUI

struct DeploymentReport {
    let componentId: String
    let componentName: String
    let success: Bool
    let output: String
    let errorMessage: String?
    let timestamp: Date
}

enum BuildError: LocalizedError {
    case noBuildCommand
    case buildFailed(Int)

    var errorDescription: String? {
        switch self {
        case .noBuildCommand:
            return "No build command configured for component"
        case .buildFailed(let exitCode):
            return "Build failed with exit code \(exitCode)"
        }
    }
}

// MARK: - CLI Response Types (match homeboy-cli output)

private struct CLIResponse<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
    let error: CLIErrorResponse?
}

private struct CLIErrorResponse: Decodable {
    let code: String
    let message: String
}

private struct CLIDeploymentResult: Decodable {
    let results: [CLIComponentResult]
    let summary: CLIDeploymentSummary
}

fileprivate struct CLIComponentResult: Decodable {
    let id: String
    let status: String
    let localVersion: String?
    let remoteVersion: String?
    let componentStatus: String?
    let error: String?
    let artifactPath: String?
    let remotePath: String?
}

private struct CLIDeploymentSummary: Decodable {
    let succeeded: Int
    let failed: Int
    let skipped: Int
}

@MainActor
class DeployerViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    @Published var components: [DeployableComponent] = []
    @Published var selectedComponents: Set<String> = []

    // Version tracking - single source of truth from CLI
    @Published fileprivate var componentData: [String: CLIComponentResult] = [:]

    @Published var sourceVersions: [String: String] = [:]
    @Published var artifactVersions: [String: String] = [:]
    @Published var remoteVersions: [String: String] = [:]

    @Published var deploymentStatus: [String: DeployStatus] = [:]
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var isDeploying = false
    @Published var isBuilding = false
    @Published var hasCredentials = false
    @Published var hasSSHKey = false
    @Published var hasBasePath = false
    @Published var serverName: String? = nil
    @Published var isCheckingConfig = true
    @Published var error: (any DisplayableError)?
    @Published var showDeployAllConfirmation = false
    @Published var showBuildConfirmation = false
    @Published var componentsNeedingBuild: [DeployableComponent] = []
    @Published var deploymentProgress: (current: Int, total: Int)? = nil
    @Published var deploymentReports: [DeploymentReport] = []
    @Published var loadError: AppError?
    @Published var failedComponentIds: [String] = []

    // MARK: - CLI Bridge

    private let cli = CLIBridge.shared

    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

    private var currentDeployTask: Task<Void, Never>?

    init() {
        checkConfiguration()
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            // Full reset on project switch
            currentDeployTask?.cancel()
            currentDeployTask = nil
            isDeploying = false
            isBuilding = false
            deploymentProgress = nil
            deploymentStatus = [:]
            consoleOutput = ""
            selectedComponents = []
            deploymentReports = []
            componentsNeedingBuild = []
            error = nil
            checkConfiguration()
            if hasCredentials && hasSSHKey && hasBasePath {
                refreshVersions()
            }
        case .projectModified(_, let fields):
            // Check if components changed
            if fields.contains(.components) {
                let project = ConfigurationManager.shared.activeProject ?? ConfigurationManager.shared.safeActiveProject
                let newComponents = ConfigurationManager.shared.loadComponentsForProject(project).map { DeployableComponent(from: $0) }
                if Set(newComponents.map(\.id)) != Set(components.map(\.id)) {
                    components = newComponents
                }
            }
            // Re-check credentials if server or basePath changed
            if fields.contains(.server) || fields.contains(.basePath) {
                checkConfiguration()
            }
        default:
            break
        }
    }
    
    func checkConfiguration() {
        let project = ConfigurationManager.shared.activeProject ?? ConfigurationManager.shared.safeActiveProject

        // Check base path (synchronous)
        hasBasePath = project.basePath != nil && !project.basePath!.isEmpty

        // Refresh component list with error tracking
        loadError = nil
        failedComponentIds = []
        let (loadedComponents, failedIds) = ConfigurationManager.shared.loadComponentsForProjectWithErrors(project)
        components = loadedComponents.map { DeployableComponent(from: $0) }
        failedComponentIds = failedIds

        // Surface load error if any components failed
        if !failedIds.isEmpty {
            loadError = AppError(
                "Failed to load \(failedIds.count) component(s): \(failedIds.joined(separator: ", "))",
                source: "Deployer"
            )
        }

        // Mark as checking before async call
        isCheckingConfig = true

        // Load server config via CLI (async)
        Task {
            await loadServerConfigFromCLI(serverId: project.serverId)
            isCheckingConfig = false
        }
    }

    private func loadServerConfigFromCLI(serverId: String?) async {
        guard let serverId = serverId else {
            hasCredentials = false
            hasSSHKey = false
            serverName = nil
            return
        }

        do {
            let server = try await HomeboyCLI.shared.serverShow(id: serverId)

            // Check credentials (host + user configured)
            hasCredentials = !server.host.isEmpty && !server.user.isEmpty
            serverName = serverId

            // Check SSH key at CLI's path (identityFile)
            if let keyPath = server.identityFile {
                hasSSHKey = FileManager.default.fileExists(atPath: keyPath)
            } else {
                hasSSHKey = false
            }
        } catch {
            hasCredentials = false
            hasSSHKey = false
            serverName = nil
        }
    }
    
    // MARK: - Version Management

    /// Hash of version data for triggering SwiftUI table re-renders.
    /// NativeDataTable doesn't observe @Published dictionaries, so we use this
    /// hash with .id() modifier to force table recreation when versions change.
    var versionDataHash: Int {
        var hasher = Hasher()
        hasher.combine(componentData.count)
        for (key, value) in componentData.sorted(by: { $0.key < $1.key }) {
            hasher.combine(key)
            hasher.combine(value.localVersion ?? "")
            hasher.combine(value.remoteVersion ?? "")
            hasher.combine(value.status)
        }
        return hasher.finalize()
    }

    func refreshVersions() {
        fetchVersions()
    }

    func fetchVersions() {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Deployer")
            return
        }

        isLoading = true
        error = nil
        consoleOutput += "> Fetching version data...\n"

        Task {
            do {
                // Use dry-run to get all version info without deploying
                let args = ["deploy", projectId, "--all", "--dry-run"]
                let response = try await cli.execute(args, timeout: 60)

                isLoading = false

                if response.success {
                    // Parse the JSON response through CLIResponse wrapper
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    if let data = response.output.data(using: .utf8),
                       let wrapper = try? decoder.decode(CLIResponse<CLIDeploymentResult>.self, from: data),
                       let result = wrapper.data {

                        // Store CLI results directly - single source of truth
                        var newData: [String: CLIComponentResult] = [:]
                        for comp in result.results {
                            newData[comp.id] = comp
                        }

                        self.componentData = newData

                        let currentCount = result.results.filter { $0.status == "current" || $0.componentStatus == "up_to_date" }.count
                        consoleOutput += "> Processed \(result.results.count) components (\(currentCount) current)\n"
                    } else {
                        consoleOutput += "> Warning: Could not parse version response\n"
                    }
                } else {
                    self.error = AppError(response.errorOutput, source: "Deployer")
                    consoleOutput += "> Error: \(response.errorOutput)\n"
                }
            } catch {
                isLoading = false
                self.error = error.toDisplayableError(source: "Deployer")
                consoleOutput += "> Error: \(error.localizedDescription)\n"
            }
        }
    }
    
    func status(for component: DeployableComponent) -> DeployStatus {
        // Check for in-progress deployment status first
        if let status = deploymentStatus[component.id] {
            return status
        }

        // Use status from CLI - single source of truth
        guard let data = componentData[component.id] else {
            return .unknown
        }

        // Map componentStatus (from CLI dry-run/check) to DeployStatus
        if let compStatus = data.componentStatus {
            switch compStatus {
            case "up_to_date":
                return .current
            case "needs_update", "behind_remote":
                return .needsUpdate
            case "not_deployed":
                return .notDeployed
            case "build_required":
                return .buildRequired
            default:
                break
            }
        }

        // Fallback to status field for deployment results
        switch data.status {
        case "deployed":
            return .current
        case "failed":
            return .failed(data.error ?? "Deployment failed")
        case "planned", "checked":
            return .needsUpdate
        case "current":
            return .current
        case "needs_update":
            return .needsUpdate
        case "not_deployed":
            return .notDeployed
        case "build_required":
            return .buildRequired
        default:
            return .unknown
        }
    }

    /// Get display string for remote version
    func remoteVersionDisplay(for component: DeployableComponent) -> String {
        componentData[component.id]?.remoteVersion ?? "—"
    }

    /// Get display string for source version
    func sourceVersionDisplay(for component: DeployableComponent) -> String {
        componentData[component.id]?.localVersion ?? "—"
    }

    /// Get display string for artifact version
    func artifactVersionDisplay(for component: DeployableComponent) -> String {
        "—"  // CLI doesn't provide separate artifact version
    }

    // MARK: - Selection
    
    func toggleSelection(_ componentId: String) {
        if selectedComponents.contains(componentId) {
            selectedComponents.remove(componentId)
        } else {
            selectedComponents.insert(componentId)
        }
    }
    
    func selectAll() {
        selectedComponents = Set(components.map { $0.id })
    }
    
    func deselectAll() {
        selectedComponents.removeAll()
    }
    
    func copyConsoleOutput() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(consoleOutput, forType: .string)
    }
    
    func selectOutdated() {
        selectedComponents = Set(components.filter { component in
            let status = self.status(for: component)
            return status == .needsUpdate || status == .notDeployed
        }.map { $0.id })
    }
    
    func selectDeployable() {
        // Select components that have a build artifact and aren't current
        selectedComponents = Set(components.filter { component in
            guard component.hasBuildArtifact else { return false }
            let status = self.status(for: component)
            return status != .current && status != .deploying
        }.map { $0.id })
    }
    
    // MARK: - Deployment

    func deploySelected() {
        let selected = components.filter { selectedComponents.contains($0.id) }
        guard !selected.isEmpty else { return }

        // Filter out components without build artifacts
        let deployable = selected.filter { $0.hasBuildArtifact }
        let skipped = selected.filter { !$0.hasBuildArtifact }

        if !skipped.isEmpty {
            consoleOutput += "> Skipping \(skipped.count) component(s) without build artifacts:\n"
            for component in skipped {
                consoleOutput += ">   - \(component.name)\n"
            }
        }

        guard !deployable.isEmpty else {
            error = AppError("No deployable components selected. Build artifacts are missing.", source: "Deployer")
            return
        }

        // Deploy directly - CLI handles version checks
        startDeployment(components: deployable)
    }

    func cancelBuild() {
        isBuilding = false
        componentsNeedingBuild = []
        showBuildConfirmation = false
    }

    /// Execute build command for a component
    private func buildComponent(
        _ component: DeployableComponent,
        onOutput: @escaping (String) -> Void
    ) async throws {
        guard let buildCommand = component.buildCommand else {
            throw BuildError.noBuildCommand
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/bash")
        process.arguments = ["-c", buildCommand]
        process.currentDirectoryURL = URL(fileURLWithPath: component.localPath)

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        // Stream output
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
        }

        return try await withCheckedThrowingContinuation { continuation in
            process.terminationHandler = { proc in
                pipe.fileHandleForReading.readabilityHandler = nil
                if proc.terminationStatus == 0 {
                    continuation.resume()
                } else {
                    continuation.resume(throwing: BuildError.buildFailed(Int(proc.terminationStatus)))
                }
            }

            do {
                try process.run()
            } catch {
                continuation.resume(throwing: error)
            }
        }
    }
    
    func confirmDeployAll() {
        showDeployAllConfirmation = true
    }
    
    func deployAll() {
        let deployable = components.filter { $0.hasBuildArtifact }
        guard !deployable.isEmpty else {
            error = AppError("No components have build artifacts", source: "Deployer")
            return
        }
        startDeployment(components: deployable)
    }
    
    func cancelDeployment() {
        currentDeployTask?.cancel()
        currentDeployTask = nil
        isDeploying = false
        deploymentProgress = nil
        consoleOutput += "\n> Deployment cancelled\n"
        deselectAll()
    }
    
    private func startDeployment(components componentsToDeploy: [DeployableComponent]) {
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Deployer")
            return
        }

        // Set initial UI state
        isDeploying = true
        error = nil
        deploymentProgress = (current: 0, total: componentsToDeploy.count)
        deploymentReports = []
        consoleOutput = "> Starting deployment of \(componentsToDeploy.count) component(s)...\n"

        // Mark all as deploying
        for component in componentsToDeploy {
            deploymentStatus[component.id] = .deploying
        }

        currentDeployTask = Task {
            let componentIds = componentsToDeploy.map { $0.id }
            let startTime = Date()

            do {
                // Build CLI command: homeboy deploy <project> --json <component1> <component2> ...
                let args = ["deploy", projectId] + componentIds

                let response = try await cli.execute(args, timeout: 300) // 5 min timeout for deployments

                if response.success {
                    // Parse JSON output through CLIResponse wrapper
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    if let data = response.output.data(using: .utf8),
                       let wrapper = try? decoder.decode(CLIResponse<CLIDeploymentResult>.self, from: data),
                       let result = wrapper.data {

                        // Convert to DeploymentReports
                        var reports: [DeploymentReport] = []
                        for comp in result.results {
                            let report = DeploymentReport(
                                componentId: comp.id,
                                componentName: comp.id,  // CLI uses id, no separate name field
                                success: comp.status == "deployed",
                                output: "",
                                errorMessage: comp.error,
                                timestamp: startTime
                            )
                            reports.append(report)

                            // Update status
                            if comp.status == "deployed" {
                                deploymentStatus[comp.id] = .current
                            } else {
                                deploymentStatus[comp.id] = .failed(comp.error ?? "Deployment failed")
                            }
                        }

                        deploymentReports = reports
                        consoleOutput = formatCLIDeploymentReport(result, output: response.output)
                    } else {
                        // Couldn't parse JSON but deployment succeeded
                        consoleOutput += response.output
                        for component in componentsToDeploy {
                            deploymentStatus[component.id] = .current
                        }
                    }
                } else {
                    // Deployment failed
                    consoleOutput += response.output
                    consoleOutput += "\n> Error: \(response.errorOutput)\n"

                    for component in componentsToDeploy {
                        deploymentStatus[component.id] = .failed(response.errorOutput)
                    }

                    self.error = AppError("Deployment failed: \(response.errorOutput)", source: "Deployer")
                }
            } catch {
                consoleOutput += "> Error: \(error.localizedDescription)\n"

                for component in componentsToDeploy {
                    deploymentStatus[component.id] = .failed(error.localizedDescription)
                }

                self.error = error.toDisplayableError(source: "Deployer")
            }

            // Finalize
            deploymentProgress = nil
            isDeploying = false
            refreshVersions()
            deselectAll()
        }
    }

    private func formatCLIDeploymentReport(_ result: CLIDeploymentResult, output: String) -> String {
        var report = ""

        for comp in result.results {
            report += "========================================\n"
            report += "> \(comp.id): \(comp.status.uppercased())\n"
            if let local = comp.localVersion, let remote = comp.remoteVersion {
                report += "> Version: \(remote) → \(local)\n"
            }
            if let error = comp.error {
                report += "> Error: \(error)\n"
            }
            report += "========================================\n\n"
        }

        report += "> Deployment complete.\n"
        report += "> \(result.summary.succeeded) succeeded, \(result.summary.failed) failed\n"

        return report
    }
    
    private func formatDeploymentReport(_ reports: [DeploymentReport]) -> String {
        var output = ""
        
        for report in reports {
            output += report.output
            output += "\n"
        }
        
        // Summary
        let succeeded = reports.filter { $0.success }.count
        let failed = reports.filter { !$0.success }.count
        
        output += "========================================\n"
        output += "> Deployment complete.\n"
        output += "> \(succeeded) succeeded, \(failed) failed\n"
        output += "========================================\n"
        
        return output
    }
}
