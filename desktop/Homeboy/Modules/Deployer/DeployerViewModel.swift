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
    @Published var sortOrder: [KeyPathComparator<DeployableComponent>] = [
        KeyPathComparator(\.name, order: .forward)
    ]
    
    var themes: [DeployableComponent] {
        components.filter { $0.type == .theme }.sorted(using: sortOrder)
    }
    
    var networkPlugins: [DeployableComponent] {
        components.filter { $0.type == .plugin && $0.isNetwork }.sorted(using: sortOrder)
    }
    
    var sitePlugins: [DeployableComponent] {
        components.filter { $0.type == .plugin && !$0.isNetwork }.sorted(using: sortOrder)
    }
    @Published var localVersions: [String: String] = [:]
    @Published var remoteVersions: [String: String] = [:]
    @Published var deploymentStatus: [String: DeployStatus] = [:]
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var isDeploying = false
    @Published var hasCredentials = false
    @Published var hasSSHKey = false
    @Published var hasDeploymentPaths = false
    @Published var serverName: String? = nil
    @Published var error: String?
    @Published var showDeployAllConfirmation = false
    @Published var deploymentProgress: (current: Int, total: Int)? = nil
    @Published var deploymentReports: [DeploymentReport] = []
    
    private var wpModule: WordPressSSHModule?
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
        
        // Check 3: wp-content path configured (for WordPress projects)
        if project.projectType == .wordpress,
           let wordpress = project.wordpress,
           wordpress.isConfigured {
            hasDeploymentPaths = true
        } else {
            hasDeploymentPaths = false
        }
        
        // Refresh component list from config
        components = ComponentRegistry.all
        
        // Initialize WordPress module if all requirements met
        if hasCredentials && hasSSHKey && hasDeploymentPaths {
            wpModule = WordPressSSHModule()
        } else {
            wpModule = nil
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
        guard let wpModule = wpModule else {
            error = "WordPress deployment not configured"
            return
        }
        
        isLoading = true
        error = nil
        consoleOutput += "> Fetching remote versions...\n"
        
        wpModule.fetchRemoteVersions(components: components) { [weak self] result in
            self?.isLoading = false
            switch result {
            case .success(let versions):
                self?.remoteVersions = versions
                self?.consoleOutput += "> Found \(versions.count) remote versions\n"
            case .failure(let err):
                self?.error = err.localizedDescription
                self?.consoleOutput += "> Error: \(err.localizedDescription)\n"
            }
        }
    }
    
    func status(for component: DeployableComponent) -> DeployStatus {
        if let status = deploymentStatus[component.id] {
            return status
        }
        
        guard let local = localVersions[component.id] else {
            return .unknown
        }
        
        guard let remote = remoteVersions[component.id] else {
            return .missing
        }
        
        if local == remote {
            return .current
        }
        
        return .needsUpdate
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
            return status == .needsUpdate || status == .missing
        }.map { $0.id })
    }
    
    // MARK: - Deployment
    
    func deploySelected() {
        let selected = components.filter { selectedComponents.contains($0.id) }
        guard !selected.isEmpty else { return }
        startDeployment(components: selected)
    }
    
    func confirmDeployAll() {
        showDeployAllConfirmation = true
    }
    
    func deployAll() {
        startDeployment(components: components)
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
        guard let module = wpModule else {
            error = "WordPress deployment not configured"
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
                await self.executeDeployment(components: componentsToDeploy, wpModule: module)
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
        wpModule: WordPressSSHModule
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
            
            do {
                // Build
                outputBuffer += "========================================\n"
                outputBuffer += "> Deploying \(component.name)...\n"
                outputBuffer += "========================================\n"
                outputBuffer += "> Building \(component.id)...\n"
                
                let (buildOutput, exitCode) = try await executeBuildProcess(at: component.localPath)
                outputBuffer += buildOutput
                
                if exitCode != 0 {
                    throw SSHError.commandFailed("Build failed with exit code \(exitCode)")
                }
                
                let zipExists = FileManager.default.fileExists(atPath: component.buildOutputPath)
                guard zipExists else {
                    throw SSHError.commandFailed("Build completed but zip not found at \(component.buildOutputPath)")
                }
                outputBuffer += "> Build complete.\n"
                
                // Deploy via WordPress module
                outputBuffer += "> Deploying to server...\n"
                
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    wpModule.deployComponent(component, buildPath: component.buildOutputPath, onOutput: { line in
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
    
    /// Executes build.sh on a background thread, returning collected output and exit status
    nonisolated private func executeBuildProcess(at directoryPath: String) async throws -> (output: String, exitCode: Int32) {
        let process = Process()
        process.currentDirectoryURL = URL(fileURLWithPath: directoryPath)
        process.executableURL = URL(fileURLWithPath: "/bin/bash")
        process.arguments = ["build.sh"]
        process.environment = [
            "PATH": "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": FileManager.default.homeDirectoryForCurrentUser.path
        ]
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        try process.run()
        process.waitUntilExit()
        
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let output = String(data: data, encoding: .utf8) ?? ""
        
        return (output, process.terminationStatus)
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
