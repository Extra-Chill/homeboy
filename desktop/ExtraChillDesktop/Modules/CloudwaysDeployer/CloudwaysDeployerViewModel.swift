import Foundation
import SwiftUI
import AppKit

struct DeploymentReport {
    let componentId: String
    let componentName: String
    let success: Bool
    let output: String
    let errorMessage: String?
    let timestamp: Date
}

@MainActor
class CloudwaysDeployerViewModel: ObservableObject {
    @Published var components: [DeployableComponent] = ComponentRegistry.all
    @Published var selectedComponents: Set<String> = []
    @Published var localVersions: [String: String] = [:]
    @Published var remoteVersions: [String: String] = [:]
    @Published var deploymentStatus: [String: DeployStatus] = [:]
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var isDeploying = false
    @Published var hasCredentials = false
    @Published var hasSSHKey = false
    @Published var hasDeploymentPaths = false
    @Published var error: String?
    @Published var showDeployAllConfirmation = false
    @Published var deploymentProgress: (current: Int, total: Int)? = nil
    @Published var deploymentReports: [DeploymentReport] = []
    
    private var sshService: SSHService?
    private var currentDeployTask: Task<Void, Never>?
    
    init() {
        checkConfiguration()
    }
    
    func checkConfiguration() {
        hasCredentials = KeychainService.hasCloudwaysCredentials()
        hasSSHKey = KeychainService.hasSSHKey()
        
        // Check deployment paths are configured and valid
        let ecPath = ComponentRegistry.extraChillBasePath
        let dmPath = ComponentRegistry.dataMachineBasePath
        hasDeploymentPaths = !ecPath.isEmpty && !dmPath.isEmpty &&
            FileManager.default.fileExists(atPath: ecPath) &&
            FileManager.default.fileExists(atPath: dmPath)
        
        if hasCredentials && hasSSHKey {
            sshService = SSHService()
        }
        
        // Refresh component list (paths may have changed)
        components = ComponentRegistry.all
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
        guard let sshService = sshService else {
            error = "SSH not configured"
            return
        }
        
        isLoading = true
        error = nil
        consoleOutput += "> Fetching remote versions...\n"
        
        sshService.fetchRemoteVersions(components: components) { [weak self] result in
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
        guard let service = sshService else {
            error = "SSH not configured"
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
                await self.executeDeployment(components: componentsToDeploy, sshService: service)
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
        sshService: SSHService
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
                
                let (buildOutput, exitCode) = try await executeBuildProcess(at: component.localFullPath)
                outputBuffer += buildOutput
                
                if exitCode != 0 {
                    throw SSHError.commandFailed("Build failed with exit code \(exitCode)")
                }
                
                let zipExists = FileManager.default.fileExists(atPath: component.buildOutputPath)
                guard zipExists else {
                    throw SSHError.commandFailed("Build completed but zip not found at \(component.buildOutputPath)")
                }
                outputBuffer += "> Build complete.\n"
                
                // Upload
                outputBuffer += "> Uploading to server...\n"
                let uploadOutput = try await sshService.uploadFileSync(
                    localPath: component.buildOutputPath,
                    remotePath: "tmp/\(component.id).zip"
                )
                if !uploadOutput.isEmpty {
                    outputBuffer += uploadOutput
                }
                outputBuffer += "> Upload complete.\n"
                
                // Remove old version
                outputBuffer += "> Removing old version...\n"
                let remotePath = "\(sshService.wpContentPath)/\(component.remotePath)"
                _ = try await sshService.executeCommandSync("rm -rf \"\(remotePath)\"")
                outputBuffer += "> Old version removed.\n"
                
                // Extract
                outputBuffer += "> Extracting...\n"
                let targetDir = component.type == .theme
                    ? "\(sshService.wpContentPath)/themes"
                    : "\(sshService.wpContentPath)/plugins"
                let extractOutput = try await sshService.executeCommandSync(
                    "unzip -o ~/tmp/\(component.id).zip -d \"\(targetDir)\" && chmod -R 755 \"\(targetDir)/\(component.id)\""
                )
                if !extractOutput.isEmpty {
                    outputBuffer += extractOutput
                }
                outputBuffer += "> Extraction complete.\n"
                
                // Cleanup
                outputBuffer += "> Cleaning up...\n"
                _ = try await sshService.executeCommandSync("rm -f ~/tmp/\(component.id).zip")
                outputBuffer += "> Cleanup complete.\n"
                
                outputBuffer += "> \(component.name) deployed successfully!\n"
                
            } catch {
                success = false
                errorMessage = error.localizedDescription
                outputBuffer += "> FAILED: \(error.localizedDescription)\n"
            }
            
            // Update component status on MainActor
            await MainActor.run {
                self.deploymentStatus[component.id] = success ? .current : .failed(errorMessage ?? "Unknown error")
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
}
