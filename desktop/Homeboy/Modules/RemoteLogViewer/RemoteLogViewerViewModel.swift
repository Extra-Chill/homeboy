import AppKit
import Combine
import Foundation
import SwiftUI

/// Represents an open log tab in the Remote Log Viewer
struct OpenLog: PinnableTabItem, Equatable {
    let id: UUID
    let path: String           // Relative path from basePath
    var isPinned: Bool
    var content: String = ""
    var fileExists: Bool = true
    var lastFetched: Date?
    var tailLines: Int
    
    var displayName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }
    
    init(id: UUID = UUID(), path: String, isPinned: Bool, tailLines: Int = 100) {
        self.id = id
        self.path = path
        self.isPinned = isPinned
        self.tailLines = tailLines
    }
    
    init(from pinned: PinnedRemoteLog) {
        self.id = pinned.id
        self.path = pinned.path
        self.isPinned = true
        self.tailLines = pinned.tailLines
    }
}

@MainActor
class RemoteLogViewerViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()
    
    // MARK: - Published State
    
    @Published var openLogs: [OpenLog] = []
    @Published var selectedLogId: UUID?
    @Published var isLoading: Bool = false
    @Published var error: AppError?
    @Published var showClearConfirmation: Bool = false

    // MARK: - Services
    
    private var sshService: SSHService?
    private var basePath: String?
    
    // MARK: - Tail Options
    
    static let tailOptions = [50, 100, 500, 1000, 0]  // 0 = All
    
    // MARK: - Computed Properties
    
    var selectedLog: OpenLog? {
        guard let id = selectedLogId else { return nil }
        return openLogs.first { $0.id == id }
    }
    
    var selectedLogIndex: Int? {
        guard let id = selectedLogId else { return nil }
        return openLogs.firstIndex { $0.id == id }
    }
    
    // MARK: - Initialization
    
    init() {
        setupSSH()
        loadPinnedLogs()
        setupSiteChangeObserver()
        observeConfiguration()
    }

    func onConfigurationChange() {
        setupSSH()
        loadPinnedLogs()
    }
    
    private func setupSSH() {
        sshService = SSHService()
        basePath = ConfigurationManager.shared.safeActiveProject.basePath
    }
    
    private func loadPinnedLogs() {
        let config = ConfigurationManager.shared.safeActiveProject
        openLogs = config.remoteLogs.pinnedLogs.map { OpenLog(from: $0) }
        
        // Select first log if available
        if let first = openLogs.first {
            selectedLogId = first.id
        }
    }
    
    // MARK: - Log Operations
    
    /// Fetches the currently selected log from the server
    func fetchSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard let ssh = sshService, let base = basePath, !base.isEmpty else {
            error = AppError("SSH or base path not configured. Check Settings.", source: "Log Viewer")
            return
        }
        
        isLoading = true
        error = nil
        
        let log = openLogs[index]
        let fullPath = "\(base)/\(log.path)"
        
        // Check if file exists
        let checkCommand = "test -f '\(fullPath)' && echo 'EXISTS' || echo 'NOTFOUND'"
        
        do {
            let checkResult = try await ssh.executeCommandSync(checkCommand)
            let exists = checkResult.trimmingCharacters(in: .whitespacesAndNewlines) == "EXISTS"
            
            openLogs[index].fileExists = exists
            
            if exists {
                // Fetch content with tail or cat
                let fetchCommand: String
                if log.tailLines > 0 {
                    fetchCommand = "tail -n \(log.tailLines) '\(fullPath)'"
                } else {
                    fetchCommand = "cat '\(fullPath)'"
                }
                
                let output = try await ssh.executeCommandSync(fetchCommand)
                openLogs[index].content = output
                openLogs[index].lastFetched = Date()
            } else {
                openLogs[index].content = ""
            }
        } catch {
            self.error = AppError(error.localizedDescription, source: "Log Viewer", path: log.displayName)
        }
        
        isLoading = false
    }
    
    /// Clears (deletes) the currently selected log file
    func clearSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard let ssh = sshService, let base = basePath, !base.isEmpty else {
            error = AppError("SSH or base path not configured. Check Settings.", source: "Log Viewer")
            return
        }
        
        isLoading = true
        error = nil
        
        let log = openLogs[index]
        let fullPath = "\(base)/\(log.path)"
        
        do {
            // Truncate the file (safer than rm)
            let clearCommand = "> '\(fullPath)'"
            _ = try await ssh.executeCommandSync(clearCommand)
            
            // Update state
            openLogs[index].content = ""
            openLogs[index].lastFetched = Date()
            
        } catch {
            self.error = AppError("Failed to clear log: \(error.localizedDescription)", source: "Log Viewer", path: log.displayName)
        }
        
        isLoading = false
    }
    
    // MARK: - Tab Management
    
    /// Selects a log tab
    func selectLog(_ id: UUID) {
        guard let log = openLogs.first(where: { $0.id == id }) else { return }
        
        selectedLogId = id
        
        // Fetch if not loaded yet
        if log.lastFetched == nil {
            Task {
                await fetchSelectedLog()
            }
        }
    }
    
    /// Opens a log from the file browser
    func openLog(path: String) {
        // Check if already open
        if let existing = openLogs.first(where: { $0.path == path }) {
            selectedLogId = existing.id
            return
        }
        
        // Create new temporary tab
        let newLog = OpenLog(path: path, isPinned: false)
        openLogs.append(newLog)
        selectedLogId = newLog.id
        
        Task {
            await fetchSelectedLog()
        }
    }
    
    /// Closes a log tab
    func closeLog(_ id: UUID) {
        guard let index = openLogs.firstIndex(where: { $0.id == id }) else { return }
        
        let log = openLogs[index]
        
        // If pinned, unpin first
        if log.isPinned {
            unpinLog(id)
        }
        
        openLogs.remove(at: index)
        
        // Select another tab if needed
        if selectedLogId == id {
            selectedLogId = openLogs.first?.id
        }
    }
    
    // MARK: - Tail Lines
    
    /// Updates tail lines for the selected log
    func setTailLines(_ lines: Int) {
        guard let index = selectedLogIndex else { return }
        openLogs[index].tailLines = lines
        
        // If pinned, update config
        if openLogs[index].isPinned {
            savePinnedLogs()
        }
        
        // Refresh with new tail count
        Task {
            await fetchSelectedLog()
        }
    }
    
    // MARK: - Pin/Unpin
    
    /// Pins a temporary log (persists to config)
    func pinLog(_ id: UUID) {
        guard let index = openLogs.firstIndex(where: { $0.id == id }) else { return }
        
        openLogs[index].isPinned = true
        savePinnedLogs()
    }
    
    /// Unpins a log (removes from config, tab stays open as temporary)
    func unpinLog(_ id: UUID) {
        guard let index = openLogs.firstIndex(where: { $0.id == id }) else { return }
        
        openLogs[index].isPinned = false
        savePinnedLogs()
    }
    
    private func savePinnedLogs() {
        let pinnedLogs = openLogs
            .filter { $0.isPinned }
            .map { PinnedRemoteLog(id: $0.id, path: $0.path, tailLines: $0.tailLines) }
        
        ConfigurationManager.shared.updateActiveProject { $0.remoteLogs.pinnedLogs = pinnedLogs }
    }
    
    // MARK: - Utility
    
    /// Copies current content to clipboard
    func copyContent() {
        guard let log = selectedLog else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(log.content, forType: .string)
    }
    
    func lastFetchedFormatted(for log: OpenLog) -> String {
        guard let date = log.lastFetched else { return "Not loaded" }
        
        let secondsAgo = Date().timeIntervalSince(date)
        if secondsAgo < 5 {
            return "Just now"
        }
        
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: Date())
    }
    
    // MARK: - Site Switching
    
    private func setupSiteChangeObserver() {
        NotificationCenter.default.publisher(for: .projectDidChange)
            .sink { [weak self] _ in
                self?.resetForSiteSwitch()
            }
            .store(in: &cancellables)
    }
    
    private func resetForSiteSwitch() {
        openLogs = []
        selectedLogId = nil
        error = nil
        
        setupSSH()
        loadPinnedLogs()
        
        // Fetch first log
        if selectedLogId != nil {
            Task {
                await fetchSelectedLog()
            }
        }
    }
}
