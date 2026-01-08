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

@MainActor
class DeployerViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    
    @Published var components: [DeployableComponent] = ComponentRegistry.all
    @Published var selectedComponents: Set<String> = []
    
    /// Components grouped by their `group` field for display (sorting handled by view)
    var groupedComponents: [(title: String, components: [DeployableComponent])] {
        Dictionary(grouping: components, by: { $0.group })
            .sorted { $0.key < $1.key }
            .map { (title: $0.key, components: $0.value) }
    }
    
    @Published var localVersions: [String: String] = [:]
    @Published var remoteVersions: [String: VersionInfo] = [:]
    @Published var deploymentStatus: [String: DeployStatus] = [:]
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var isDeploying = false
    @Published var hasCredentials = false
    @Published var hasSSHKey = false
    @Published var hasBasePath = false
    @Published var serverName: String? = nil
    @Published var error: AppError?
    @Published var showDeployAllConfirmation = false
    @Published var deploymentProgress: (current: Int, total: Int)? = nil
    @Published var deploymentReports: [DeploymentReport] = []
    
    private var deploymentService: DeploymentService?
    private var currentDeployTask: Task<Void, Never>?
    
    init() {
        checkConfiguration()
    }
    
    func checkConfiguration() {
        let project = ConfigurationManager.readCurrentProject()
        
        // Check 1: Server credentials (host + user configured)
        if let serverId = project.serverId,
           let server = ConfigurationManager.readServer(id: serverId),
           !server.host.isEmpty,
           !server.user.isEmpty {
            hasCredentials = true
            serverName = server.name
        } else {
            hasCredentials = false
            serverName = nil
        }
        
        // Check 2: SSH key exists for the server
        if let serverId = project.serverId {
            hasSSHKey = KeychainService.hasSSHKey(forServer: serverId)
        } else {
            hasSSHKey = false
        }
        
        // Check 3: Base path configured
        hasBasePath = project.basePath != nil && !project.basePath!.isEmpty
        
        // Refresh component list from config
        components = ComponentRegistry.all
        
        // Initialize deployment service if all requirements met
        if hasCredentials && hasSSHKey && hasBasePath {
            deploymentService = DeploymentService(project: project)
        } else {
            deploymentService = nil
        }
    }
    
    // MARK: - Version Management
    
    func refreshVersions() {
        loadLocalVersions()
        fetchRemoteVersions()
    }
    
    func loadLocalVersions() {
        for component in components {
            if let version = VersionParser.parseLocalVersion(for: component) {
                localVersions[component.id] = version
            }
        }
    }
    
    func fetchRemoteVersions() {
        guard let service = deploymentService else {
            error = AppError("Deployment not configured", source: "Deployer")
            return
        }
        
        isLoading = true
        error = nil
        consoleOutput += "> Fetching remote versions...\n"
        
        service.fetchRemoteVersions(components: components) { [weak self] result in
            DispatchQueue.main.async {
                self?.isLoading = false
                switch result {
                case .success(let versions):
                    self?.remoteVersions = versions
                    self?.consoleOutput += "> Found \(versions.count) remote versions\n"
                case .failure(let err):
                    self?.error = AppError(err.localizedDescription, source: "Deployer")
                    self?.consoleOutput += "> Error: \(err.localizedDescription)\n"
                }
            }
        }
    }
    
    func status(for component: DeployableComponent) -> DeployStatus {
        // Check for in-progress deployment status first
        if let status = deploymentStatus[component.id] {
            return status
        }
        
        // Check if build artifact exists
        guard component.hasBuildArtifact else {
            return .buildRequired
        }
        
        // Check local version
        let localVersion = localVersions[component.id]
        
        // Check remote version
        guard let remoteInfo = remoteVersions[component.id] else {
            return .unknown
        }
        
        switch remoteInfo {
        case .notDeployed:
            return .notDeployed
            
        case .version(let remoteVersion):
            guard let local = localVersion else {
                return .unknown
            }
            return local == remoteVersion ? .current : .needsUpdate
            
        case .timestamp:
            // Can't compare versions with timestamps, show as unknown
            // User can still deploy manually
            return .unknown
        }
    }
    
    /// Get display string for remote version
    func remoteVersionDisplay(for component: DeployableComponent) -> String {
        guard let info = remoteVersions[component.id] else {
            return "—"
        }
        return info.displayString
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
        
        startDeployment(components: deployable)
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
        guard let service = deploymentService else {
            error = AppError("Deployment not configured", source: "Deployer")
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
            // Run entirely detached from MainActor
            let reports = await Task.detached(priority: .userInitiated) {
                await self.executeDeployment(components: componentsToDeploy, service: service)
            }.value
            
            // Back on MainActor - update UI with results
            deploymentReports = reports
            deploymentProgress = nil
            isDeploying = false
            consoleOutput = formatDeploymentReport(reports)
            refreshVersions()
            deselectAll()
        }
    }
    
    /// Executes deployment entirely in background, returning reports when complete
    nonisolated private func executeDeployment(
        components: [DeployableComponent],
        service: DeploymentService
    ) async -> [DeploymentReport] {
        var reports: [DeploymentReport] = []
        
        for (index, component) in components.enumerated() {
            // Update progress on MainActor (minimal UI touch)
            await MainActor.run {
                self.deploymentProgress = (current: index + 1, total: components.count)
            }
            
            var outputBuffer = ""
            let startTime = Date()
            var success = true
            var errorMessage: String? = nil
            
            outputBuffer += "========================================\n"
            outputBuffer += "> Deploying \(component.name)...\n"
            outputBuffer += "========================================\n"
            
            // Verify artifact exists (belt and suspenders)
            guard FileManager.default.fileExists(atPath: component.buildArtifactPath) else {
                success = false
                errorMessage = "Build artifact not found at \(component.buildArtifactPath)"
                outputBuffer += "> FAILED: \(errorMessage!)\n"
                
                let capturedErrorMessage = errorMessage
                await MainActor.run {
                    self.deploymentStatus[component.id] = .failed(capturedErrorMessage!)
                }
                
                reports.append(DeploymentReport(
                    componentId: component.id,
                    componentName: component.name,
                    success: false,
                    output: outputBuffer,
                    errorMessage: errorMessage,
                    timestamp: startTime
                ))
                continue
            }
            
            outputBuffer += "> Artifact: \(component.buildArtifact)\n"
            
            // Deploy via DeploymentService
            do {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    service.deploy(component: component, onOutput: { line in
                        outputBuffer += line
                    }) { result in
                        switch result {
                        case .success:
                            continuation.resume()
                        case .failure(let error):
                            continuation.resume(throwing: error)
                        }
                    }
                }
                
                outputBuffer += "> \(component.name) deployed successfully!\n"
                
            } catch {
                success = false
                errorMessage = error.localizedDescription
                outputBuffer += "> FAILED: \(error.localizedDescription)\n"
            }
            
            // Capture final values before entering @Sendable MainActor closure
            let finalSuccess = success
            let finalErrorMessage = errorMessage
            
            // Update component status on MainActor
            await MainActor.run {
                self.deploymentStatus[component.id] = finalSuccess ? .current : .failed(finalErrorMessage ?? "Unknown error")
            }
            
            reports.append(DeploymentReport(
                componentId: component.id,
                componentName: component.name,
                success: success,
                output: outputBuffer,
                errorMessage: errorMessage,
                timestamp: startTime
            ))
        }
        
        return reports
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
    
    // MARK: - Site Switching
    
    func setupSiteChangeObserver() {
        NotificationCenter.default.publisher(for: .projectDidChange)
            .sink { [weak self] _ in
                self?.resetForSiteSwitch()
            }
            .store(in: &cancellables)
    }
    
    private func resetForSiteSwitch() {
        // Clear cached data
        localVersions = [:]
        remoteVersions = [:]
        deploymentStatus = [:]
        consoleOutput = ""
        selectedComponents = []
        deploymentReports = []
        error = nil
        
        // Re-check configuration for new site
        checkConfiguration()
    }
}
