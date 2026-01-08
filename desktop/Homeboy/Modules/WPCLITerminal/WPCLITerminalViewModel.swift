import AppKit
import Combine
import Foundation
import SwiftUI

enum TerminalEnvironment: String, CaseIterable {
    case local = "Local"
    case production = "Production"
}

struct NetworkSite: Identifiable, Hashable {
    let id: String
    let name: String
    let blogId: Int
    let domain: String
    
    func urlFlag(localDomain: String) -> String {
        if blogId == 1 {
            return localDomain
        }
        return "\(localDomain)/\(name.lowercased())"
    }
}

@MainActor
class WPCLITerminalViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    
    @Published var command = ""
    @Published var output = ""
    @Published var isRunning = false
    @Published var commandHistory: [String] = []
    @Published var historyIndex = -1
    @Published var environment: TerminalEnvironment = .local
    
    @AppStorage("selectedSiteId") private var selectedSiteId = "main"
    
    private var process: Process?
    private var sshService: SSHService?
    private let configManager = ConfigurationManager.shared
    
    var localWPPath: String {
        configManager.activeProject.localDev.wpCliPath
    }
    
    var localDomain: String {
        let domain = configManager.activeProject.localDev.domain
        return domain.isEmpty ? "localhost" : domain
    }
    
    var isMultisite: Bool {
        configManager.activeProject.multisite?.enabled ?? false
    }
    
    var networkSites: [NetworkSite] {
        guard let multisite = configManager.activeProject.multisite,
              multisite.enabled else {
            return [NetworkSite(
                id: "main",
                name: configManager.activeProject.name,
                blogId: 1,
                domain: configManager.activeProject.domain
            )]
        }
        
        return multisite.blogs.map { blog in
            NetworkSite(
                id: blog.name.lowercased(),
                name: blog.name,
                blogId: blog.blogId,
                domain: blog.domain
            )
        }
    }
    
    var selectedSite: NetworkSite {
        networkSites.first { $0.id == selectedSiteId } ?? networkSites[0]
    }
    
    func selectSite(_ site: NetworkSite) {
        selectedSiteId = site.id
    }
    
    // MARK: - Production Environment Helpers
    
    var productionDomain: String {
        configManager.activeProject.domain
    }
    
    /// Returns the production URL for the selected multisite blog
    var productionUrlFlag: String {
        guard let multisite = configManager.activeProject.multisite,
              multisite.enabled,
              let blog = multisite.blogs.first(where: { $0.name.lowercased() == selectedSiteId }) else {
            return productionDomain
        }
        return blog.domain
    }
    
    /// Returns the remote WordPress app path (wp-content parent)
    var productionAppPath: String {
        guard let wpContentPath = configManager.activeProject.wordpress?.wpContentPath else {
            return ""
        }
        if wpContentPath.hasSuffix("/wp-content") {
            return String(wpContentPath.dropLast("/wp-content".count))
        }
        return wpContentPath
    }
    
    /// Whether production mode is available (SSH configured)
    var isProductionConfigured: Bool {
        SSHService.isConfigured()
    }
    
    /// Whether the production site is multisite
    var hasProductionMultisite: Bool {
        configManager.activeProject.multisite?.enabled ?? false
    }
    
    // MARK: - Command Execution
    
    func runCommand() {
        guard !command.isEmpty, !isRunning else { return }
        
        if commandHistory.last != command {
            commandHistory.append(command)
        }
        historyIndex = commandHistory.count
        
        let cmd = command
        command = ""
        
        switch environment {
        case .local:
            executeLocalCommand(cmd)
        case .production:
            executeProductionCommand(cmd)
        }
    }
    
    private func executeLocalCommand(_ cmd: String) {
        isRunning = true
        let urlFlag = selectedSite.urlFlag(localDomain: localDomain)
        output += "$ \(cmd) --url=\(urlFlag)\n"
        
        guard let environment = LocalEnvironment.buildEnvironment() else {
            output += "Error: Local by Flywheel PHP not detected.\n"
            output += "Please ensure Local is installed and has a PHP version available.\n"
            output += "Expected path: ~/Library/Application Support/Local/lightning-services/php-*/\n"
            isRunning = false
            return
        }
        
        let process = Process()
        self.process = process
        
        process.currentDirectoryURL = URL(fileURLWithPath: localWPPath)
        process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
        process.environment = environment
        
        var args = cmd.replacingOccurrences(of: "wp ", with: "")
            .components(separatedBy: " ")
            .filter { !$0.isEmpty }
        
        // Append --url flag for multisite targeting
        args.append("--url=\(urlFlag)")
        
        process.arguments = args
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                }
            }
        }
        
        process.terminationHandler = { [weak self] _ in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            let remainingData = pipe.fileHandleForReading.readDataToEndOfFile()
            let remainingOutput = String(data: remainingData, encoding: .utf8) ?? ""
            
            DispatchQueue.main.async {
                if !remainingOutput.isEmpty {
                    self?.output += remainingOutput
                }
                self?.output += "\n"
                self?.isRunning = false
                self?.process = nil
            }
        }
        
        do {
            try process.run()
        } catch {
            output += "Error: \(error.localizedDescription)\n"
            isRunning = false
        }
    }
    
    private func executeProductionCommand(_ cmd: String) {
        isRunning = true
        let urlFlag = productionUrlFlag
        output += "[\(productionDomain)] $ \(cmd) --url=\(urlFlag)\n"
        
        // Ensure SSH service is initialized
        if sshService == nil {
            sshService = SSHService()
        }
        
        guard let ssh = sshService else {
            output += "Error: SSH not configured. Check Settings > SSH.\n"
            isRunning = false
            return
        }
        
        // Build remote WP-CLI command
        var args = cmd
        if args.lowercased().hasPrefix("wp ") {
            args = String(args.dropFirst(3))
        }
        
        let remoteCommand = "wp \(args) --path=\(productionAppPath) --url=\(urlFlag)"
        
        ssh.executeCommand(remoteCommand, onOutput: { [weak self] line in
            self?.output += line
        }) { [weak self] result in
            switch result {
            case .success:
                // Output already streamed via onOutput
                break
            case .failure(let error):
                self?.output += "Error: \(error.localizedDescription)\n"
            }
            self?.output += "\n"
            self?.isRunning = false
        }
    }
    
    func cancelCommand() {
        process?.terminate()
        isRunning = false
    }
    
    func clearOutput() {
        output = ""
    }
    
    func copyOutput() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(output, forType: .string)
    }
    
    func navigateHistory(direction: Int) {
        let newIndex = historyIndex + direction
        
        if newIndex >= 0 && newIndex < commandHistory.count {
            historyIndex = newIndex
            command = commandHistory[newIndex]
        } else if newIndex >= commandHistory.count {
            historyIndex = commandHistory.count
            command = ""
        }
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
        // Clear output and history
        output = ""
        commandHistory = []
        historyIndex = -1
        command = ""
        
        // Reset site selection to main
        selectedSiteId = "main"
        
        // Reset environment to local and clear SSH service for new project
        environment = .local
        sshService = nil
    }
}
