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
    let components: [CLIComponentResult]
    let summary: CLIDeploymentSummary
}

private struct CLIComponentResult: Decodable {
    let id: String
    let name: String
    let status: String
    let duration: Double?
    let sourceVersion: String?
    let artifactVersion: String?
    let remoteVersion: String?
    let error: String?
}

private struct CLIDeploymentSummary: Decodable {
    let succeeded: Int
    let failed: Int
    let skipped: Int
}

@MainActor
class DeployerViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    @Published var components: [DeployableComponent] = ConfigurationManager.loadComponentsForProject(ConfigurationManager.readCurrentProject()).map { DeployableComponent(from: $0) }
    @Published var selectedComponents: Set<String> = []

    // Component groupings (universal system)
    @Published var groupedComponents: [(grouping: ItemGrouping, components: [DeployableComponent], isExpanded: Bool)] = []
    @Published var ungroupedComponents: [DeployableComponent] = []
    @Published var isUngroupedExpanded: Bool = true

    private var currentGroupings: [ItemGrouping] = []

    // Version tracking - single source of truth from CLI
    @Published var componentData: [String: CLIComponentResult] = [:]

    @Published var deploymentStatus: [String: DeployStatus] = [:]
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var isDeploying = false
    @Published var isBuilding = false
    @Published var hasCredentials = false
    @Published var hasSSHKey = false
    @Published var hasBasePath = false
    @Published var serverName: String? = nil
    @Published var error: AppError?
    @Published var showDeployAllConfirmation = false
    @Published var showBuildConfirmation = false
    @Published var componentsNeedingBuild: [DeployableComponent] = []
    @Published var deploymentProgress: (current: Int, total: Int)? = nil
    @Published var deploymentReports: [DeploymentReport] = []

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
            sourceVersions = [:]
            artifactVersions = [:]
            remoteVersions = [:]
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
            // Check if components or groupings changed
            if fields.contains(.components) || fields.contains(.componentGroupings) {
                let project = ConfigurationManager.readCurrentProject()
                let newComponents = ConfigurationManager.loadComponentsForProject(project).map { DeployableComponent(from: $0) }
                if Set(newComponents.map(\.id)) != Set(components.map(\.id)) {
                    components = newComponents
                }
                currentGroupings = ConfigurationManager.readCurrentProject().componentGroupings
                refreshGroupedComponents()
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
        let project = ConfigurationManager.readCurrentProject()

        // Check 1: Server credentials (host + user configured)
        if let serverId = project.serverId,
           let server = ConfigurationManager.readServer(id: serverId),
           server.isValid {
            hasCredentials = true
            serverName = server.name
        } else {
            hasCredentials = false
            serverName = nil
        }

        // Check 2: SSH key exists for the server
        if let serverId = project.serverId {
            hasSSHKey = SSHKeyManager.hasKeyFile(forServer: serverId)
        } else {
            hasSSHKey = false
        }

        // Check 3: Base path configured
        hasBasePath = project.basePath != nil && !project.basePath!.isEmpty

        // Refresh component list from config
        components = ConfigurationManager.loadComponentsForProject(project).map { DeployableComponent(from: $0) }

        // Load groupings from config
        currentGroupings = project.componentGroupings
        refreshGroupedComponents()
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
            hasher.combine(value.sourceVersion ?? "")
            hasher.combine(value.artifactVersion ?? "")
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
                    if let data = response.output.data(using: .utf8),
                       let wrapper = try? JSONDecoder().decode(CLIResponse<CLIDeploymentResult>.self, from: data),
                       let result = wrapper.data {

                        // Store CLI results directly - single source of truth
                        var newData: [String: CLIComponentResult] = [:]
                        for comp in result.components {
                            newData[comp.id] = comp
                        }

                        self.componentData = newData

                        let currentCount = result.components.filter { $0.status == "current" }.count
                        consoleOutput += "> Processed \(result.components.count) components (\(currentCount) current)\n"
                    } else {
                        consoleOutput += "> Warning: Could not parse version response\n"
                    }
                } else {
                    self.error = AppError(response.errorOutput, source: "Deployer")
                    consoleOutput += "> Error: \(response.errorOutput)\n"
                }
            } catch {
                isLoading = false
                self.error = AppError(error.localizedDescription, source: "Deployer")
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

        // Map CLI status strings to DeployStatus enum
        switch data.status {
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
        componentData[component.id]?.sourceVersion ?? "—"
    }

    /// Get display string for artifact version
    func artifactVersionDisplay(for component: DeployableComponent) -> String {
        componentData[component.id]?.artifactVersion ?? "—"
    }

    /// Check if source and artifact versions match for a component
    func versionsMatch(for component: DeployableComponent) -> Bool {
        guard let data = componentData[component.id],
              let source = data.sourceVersion,
              let artifact = data.artifactVersion else {
            return true  // If we can't detect, assume match
        }
        return source == artifact
    }

    /// Get components that have version mismatches (source != artifact)
    func componentsMismatchedVersions(_ selectedComponents: [DeployableComponent]) -> [DeployableComponent] {
        selectedComponents.filter { !versionsMatch(for: $0) }
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

        // Check for version mismatches (source != artifact)
        let mismatched = componentsMismatchedVersions(deployable)

        if mismatched.isEmpty {
            // All versions match, deploy directly
            startDeployment(components: deployable)
        } else {
            // Version mismatch detected - check if components have build commands
            let withoutBuildCommand = mismatched.filter { !$0.hasBuildCommand }

            if !withoutBuildCommand.isEmpty {
                // Block deployment with error - no build command configured
                let names = withoutBuildCommand.map { $0.name }.joined(separator: ", ")
                error = AppError(
                    "Cannot deploy: Version mismatch for \(names) but no build command configured. Rebuild manually or add buildCommand to component config.",
                    source: "Deployer"
                )
                return
            }

            // All mismatched components have build commands - show confirmation
            componentsNeedingBuild = mismatched
            showBuildConfirmation = true
        }
    }

    /// Called when user confirms build before deploy
    func confirmBuildAndDeploy() {
        showBuildConfirmation = false

        Task {
            isBuilding = true
            consoleOutput += "> Building \(componentsNeedingBuild.count) component(s)...\n"

            for component in componentsNeedingBuild {
                consoleOutput += "> Building \(component.name)...\n"
                do {
                    try await buildComponent(component) { [weak self] line in
                        Task { @MainActor in
                            self?.consoleOutput += line
                        }
                    }
                    consoleOutput += "> Build complete for \(component.name)\n"
                } catch {
                    self.error = AppError("Build failed for \(component.name): \(error.localizedDescription)", source: "Deployer")
                    isBuilding = false
                    componentsNeedingBuild = []
                    return
                }
            }

            isBuilding = false

            // Reload artifact versions after build
            loadArtifactVersions()

            // Verify versions now match
            let stillMismatched = componentsMismatchedVersions(componentsNeedingBuild)
            if !stillMismatched.isEmpty {
                let names = stillMismatched.map { $0.name }.joined(separator: ", ")
                error = AppError("Build did not update artifact versions for: \(names)", source: "Deployer")
                componentsNeedingBuild = []
                return
            }

            // Proceed with deployment
            let selected = components.filter { selectedComponents.contains($0.id) }
            let deployable = selected.filter { $0.hasBuildArtifact }
            componentsNeedingBuild = []
            startDeployment(components: deployable)
        }
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
                    if let data = response.output.data(using: .utf8),
                       let wrapper = try? JSONDecoder().decode(CLIResponse<CLIDeploymentResult>.self, from: data),
                       let result = wrapper.data {

                        // Convert to DeploymentReports
                        var reports: [DeploymentReport] = []
                        for comp in result.components {
                            let report = DeploymentReport(
                                componentId: comp.id,
                                componentName: comp.name,
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

                self.error = AppError(error.localizedDescription, source: "Deployer")
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

        for comp in result.components {
            report += "========================================\n"
            report += "> \(comp.name): \(comp.status.uppercased())\n"
            if let local = comp.localVersion, let remote = comp.remoteVersion {
                report += "> Version: \(remote) → \(local)\n"
            }
            if let duration = comp.duration {
                report += "> Duration: \(String(format: "%.1f", duration))s\n"
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
    
    // MARK: - Grouping Management
    
    /// Create a new grouping from component IDs
    func createGrouping(name: String, fromComponentIds componentIds: [String]) {
        let newGrouping = GroupingManager.createGrouping(
            name: name,
            fromIds: componentIds,
            existingGroupings: currentGroupings
        )
        currentGroupings.append(newGrouping)
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Add components to an existing grouping
    func addComponentsToGrouping(componentIds: [String], groupingId: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index] = GroupingManager.addMembers(componentIds, to: currentGroupings[index])
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Remove components from a grouping
    func removeComponentsFromGrouping(componentIds: [String], groupingId: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index] = GroupingManager.removeMembers(componentIds, from: currentGroupings[index])
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Rename a grouping
    func renameGrouping(groupingId: String, newName: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index].name = newName
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Delete a grouping (components become ungrouped)
    func deleteGrouping(groupingId: String) {
        currentGroupings = GroupingManager.deleteGrouping(id: groupingId, from: currentGroupings)
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Move a grouping up in the list
    func moveGroupingUp(groupingId: String) {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }),
              index > 0 else { return }
        currentGroupings = GroupingManager.moveGrouping(in: currentGroupings, fromIndex: index, toIndex: index - 1)
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Move a grouping down in the list
    func moveGroupingDown(groupingId: String) {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }),
              index < sorted.count - 1 else { return }
        currentGroupings = GroupingManager.moveGrouping(in: currentGroupings, fromIndex: index, toIndex: index + 1)
        saveGroupings()
        refreshGroupedComponents()
    }
    
    /// Check if a grouping can be moved up
    func canMoveGroupingUp(groupingId: String) -> Bool {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }) else { return false }
        return index > 0
    }
    
    /// Check if a grouping can be moved down
    func canMoveGroupingDown(groupingId: String) -> Bool {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }) else { return false }
        return index < sorted.count - 1
    }
    
    /// Toggle expansion state for a grouping
    func toggleGroupExpansion(groupingId: String) {
        guard let index = groupedComponents.firstIndex(where: { $0.grouping.id == groupingId }) else { return }
        groupedComponents[index].isExpanded.toggle()
    }
    
    /// Toggle expansion state for ungrouped section
    func toggleUngroupedExpansion() {
        isUngroupedExpanded.toggle()
    }
    
    /// Find which grouping a component belongs to (by explicit membership)
    func groupingForComponent(_ componentId: String) -> ItemGrouping? {
        currentGroupings.first { $0.memberIds.contains(componentId) }
    }
    
    /// Check if a component is in a group by explicit membership
    func isComponentInGroupByMembership(_ componentId: String) -> Bool {
        currentGroupings.contains { $0.memberIds.contains(componentId) }
    }
    
    /// Get all available groupings for adding components
    var availableGroupings: [ItemGrouping] {
        currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
    }
    
    // MARK: - Private Helpers
    
    private func saveGroupings() {
        let groupings = currentGroupings
        ConfigurationManager.shared.updateActiveProject { project in
            project.componentGroupings = groupings
        }
    }
    
    private func refreshGroupedComponents() {
        let result = GroupingManager.categorize(
            items: components,
            groupings: currentGroupings,
            idExtractor: { $0.id }
        )
        
        // Preserve expansion state where possible
        let oldExpansionState = Dictionary(uniqueKeysWithValues: groupedComponents.map { ($0.grouping.id, $0.isExpanded) })
        
        groupedComponents = result.grouped.map { (grouping, items) in
            let wasExpanded = oldExpansionState[grouping.id] ?? true
            return (grouping: grouping, components: items, isExpanded: wasExpanded)
        }
        ungroupedComponents = result.ungrouped
    }
}
