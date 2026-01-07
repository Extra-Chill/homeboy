import Foundation
import SwiftUI
import AppKit

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
        
        currentDeployTask = Task {
            await deployComponents(selected)
        }
    }
    
    func confirmDeployAll() {
        showDeployAllConfirmation = true
    }
    
    func deployAll() {
        currentDeployTask = Task {
            await deployComponents(components)
        }
    }
    
    func cancelDeployment() {
        currentDeployTask?.cancel()
        currentDeployTask = nil
        isDeploying = false
        consoleOutput += "\n> Deployment cancelled\n"
        deselectAll()
    }
    
    private func deployComponents(_ componentsToDeploy: [DeployableComponent]) async {
        guard let sshService = sshService else {
            error = "SSH not configured"
            return
        }
        
        isDeploying = true
        error = nil
        
        for component in componentsToDeploy {
            if Task.isCancelled { break }
            
            deploymentStatus[component.id] = .deploying
            consoleOutput += "\n========================================\n"
            consoleOutput += "> Deploying \(component.name)...\n"
            consoleOutput += "========================================\n"
            
            do {
                try await deployComponent(component, sshService: sshService)
                deploymentStatus[component.id] = .current
                consoleOutput += "> \(component.name) deployed successfully!\n"
            } catch {
                deploymentStatus[component.id] = .failed(error.localizedDescription)
                consoleOutput += "> FAILED: \(error.localizedDescription)\n"
            }
        }
        
        isDeploying = false
        consoleOutput += "\n> Deployment complete. Refreshing versions...\n"
        refreshVersions()
        deselectAll()
    }
    
    private func deployComponent(_ component: DeployableComponent, sshService: SSHService) async throws {
        // Step 1: Build
        consoleOutput += "> Building \(component.id)...\n"
        try await runBuild(for: component)
        
        // Step 2: Upload
        consoleOutput += "> Uploading to server...\n"
        try await uploadZip(for: component, sshService: sshService)
        
        // Step 3: Remove old version
        consoleOutput += "> Removing old version...\n"
        try await removeOldVersion(for: component, sshService: sshService)
        
        // Step 4: Extract
        consoleOutput += "> Extracting...\n"
        try await extractZip(for: component, sshService: sshService)
        
        // Step 5: Cleanup
        consoleOutput += "> Cleaning up...\n"
        try await cleanupRemoteZip(for: component, sshService: sshService)
    }
    
    private func runBuild(for component: DeployableComponent) async throws {
        // Continuation returns remaining output to avoid DispatchQueue.main.sync deadlock
        let remainingOutput: String = try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
            let process = Process()
            process.currentDirectoryURL = URL(fileURLWithPath: component.localFullPath)
            process.executableURL = URL(fileURLWithPath: "/bin/bash")
            process.arguments = ["build.sh"]
            
            // Set PATH to include Homebrew so composer, npm, etc. are available
            process.environment = [
                "PATH": "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                "HOME": FileManager.default.homeDirectoryForCurrentUser.path
            ]
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
                let data = handle.availableData
                if let line = String(data: data, encoding: .utf8), !line.isEmpty {
                    DispatchQueue.main.async {
                        self?.consoleOutput += line
                    }
                }
            }
            
            do {
                try process.run()
                process.waitUntilExit()
                
                // Clear handler and read remaining data synchronously
                pipe.fileHandleForReading.readabilityHandler = nil
                let remainingData = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: remainingData, encoding: .utf8) ?? ""
                
                if process.terminationStatus == 0 {
                    // Verify zip exists
                    if FileManager.default.fileExists(atPath: component.buildOutputPath) {
                        continuation.resume(returning: output)
                    } else {
                        continuation.resume(throwing: SSHError.commandFailed("Build completed but zip not found at \(component.buildOutputPath)"))
                    }
                } else {
                    continuation.resume(throwing: SSHError.commandFailed("Build failed with exit code \(process.terminationStatus)"))
                }
            } catch {
                continuation.resume(throwing: error)
            }
        }
        
        // UI updates happen here, safely on @MainActor after continuation completes
        if !remainingOutput.isEmpty {
            consoleOutput += remainingOutput
        }
        consoleOutput += "> Build complete.\n"
    }
    
    private func uploadZip(for component: DeployableComponent, sshService: SSHService) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            sshService.uploadFile(
                localPath: component.buildOutputPath,
                remotePath: "tmp/\(component.id).zip",
                onOutput: { [weak self] line in
                    self?.consoleOutput += line
                },
                onComplete: { result in
                    switch result {
                    case .success:
                        continuation.resume()
                    case .failure(let error):
                        continuation.resume(throwing: error)
                    }
                }
            )
        }
    }
    
    private func removeOldVersion(for component: DeployableComponent, sshService: SSHService) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let remotePath = "\(sshService.wpContentPath)/\(component.remotePath)"
            sshService.executeCommand(
                "rm -rf \"\(remotePath)\"",
                onOutput: { [weak self] line in
                    self?.consoleOutput += line
                },
                onComplete: { result in
                    switch result {
                    case .success:
                        continuation.resume()
                    case .failure(let error):
                        continuation.resume(throwing: error)
                    }
                }
            )
        }
    }
    
    private func extractZip(for component: DeployableComponent, sshService: SSHService) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let targetDir: String
            if component.type == .theme {
                targetDir = "\(sshService.wpContentPath)/themes"
            } else {
                targetDir = "\(sshService.wpContentPath)/plugins"
            }
            
            sshService.executeCommand(
                "unzip -o ~/tmp/\(component.id).zip -d \"\(targetDir)\" && chmod -R 755 \"\(targetDir)/\(component.id)\"",
                onOutput: { [weak self] line in
                    self?.consoleOutput += line
                },
                onComplete: { result in
                    switch result {
                    case .success:
                        continuation.resume()
                    case .failure(let error):
                        continuation.resume(throwing: error)
                    }
                }
            )
        }
    }
    
    private func cleanupRemoteZip(for component: DeployableComponent, sshService: SSHService) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            sshService.executeCommand(
                "rm -f ~/tmp/\(component.id).zip",
                onOutput: nil,
                onComplete: { result in
                    switch result {
                    case .success:
                        continuation.resume()
                    case .failure(let error):
                        continuation.resume(throwing: error)
                    }
                }
            )
        }
    }
}
