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

// MARK: - CLI Response Types

/// Response from `logs list --json`
private struct LogsListResponse: Decodable {
    let label: String
    let path: String
    let tailLines: Int
}

/// Response from `logs clear --json`
private struct LogsClearResponse: Decodable {
    let success: Bool
    let path: String
    let label: String
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

    // MARK: - CLI Bridge

    private let cli = CLIBridge.shared

    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

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
        loadPinnedLogs()
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            // Full reset on project switch
            openLogs = []
            selectedLogId = nil
            error = nil
            loadPinnedLogs()
            if selectedLogId != nil {
                Task {
                    await fetchSelectedLog()
                }
            }
        case .projectModified(_, let fields):
            // Reload pinned logs if remoteLogs changed
            if fields.contains(.remoteLogs) {
                loadPinnedLogs()
            }
        default:
            break
        }
    }

    private func loadPinnedLogs() {
        // Load pinned logs from config (still uses ConfigurationManager for initial data)
        let config = ConfigurationManager.shared.safeActiveProject
        openLogs = config.remoteLogs.pinnedLogs.map { OpenLog(from: $0) }

        // Select first log if available
        if let first = openLogs.first {
            selectedLogId = first.id
        }
    }

    // MARK: - Log Operations

    /// Fetches the currently selected log from the server via CLI
    func fetchSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        isLoading = true
        error = nil

        let log = openLogs[index]

        do {
            // Build command: homeboy logs show <project> <logName> -n <lines>
            var args = ["logs", "show", projectId, log.displayName]
            if log.tailLines > 0 {
                args.append(contentsOf: ["-n", String(log.tailLines)])
            }

            let response = try await cli.execute(args, timeout: 60)

            if response.success {
                openLogs[index].content = response.output
                openLogs[index].fileExists = true
                openLogs[index].lastFetched = Date()
            } else {
                // Check if error indicates file not found
                if response.errorOutput.contains("not found") || response.errorOutput.contains("No such file") {
                    openLogs[index].fileExists = false
                    openLogs[index].content = ""
                } else {
                    self.error = AppError(response.errorOutput, source: "Log Viewer", path: log.displayName)
                }
            }
        } catch {
            self.error = AppError(error.localizedDescription, source: "Log Viewer", path: log.displayName)
        }

        isLoading = false
    }

    /// Clears the currently selected log file via CLI
    func clearSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        isLoading = true
        error = nil

        let log = openLogs[index]

        do {
            // homeboy logs clear <project> <logName> --json
            let args = ["logs", "clear", projectId, log.displayName]
            let response = try await cli.execute(args, timeout: 30)

            if response.success {
                // Update state
                openLogs[index].content = ""
                openLogs[index].lastFetched = Date()
            } else {
                self.error = AppError("Failed to clear log: \(response.errorOutput)", source: "Log Viewer", path: log.displayName)
            }
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
        let oldLines = openLogs[index].tailLines
        openLogs[index].tailLines = lines

        // If pinned, update config via CLI (remove and re-add with new tail lines)
        if openLogs[index].isPinned && cli.isInstalled {
            let log = openLogs[index]
            Task {
                do {
                    // Remove old pin
                    let removeArgs = ["pin", "remove", projectId, log.path, "--type", "log"]
                    let removeResponse = try await cli.execute(removeArgs, timeout: 30)

                    if removeResponse.success {
                        // Re-add with new tail lines
                        let addArgs = ["pin", "add", projectId, log.path, "--type", "log", "--tail", String(lines)]
                        let addResponse = try await cli.execute(addArgs, timeout: 30)

                        if !addResponse.success {
                            // Rollback local state
                            openLogs[index].tailLines = oldLines
                            self.error = AppError("Failed to update tail lines: \(addResponse.errorOutput)", source: "Log Viewer")
                        }
                    } else {
                        // Rollback local state
                        openLogs[index].tailLines = oldLines
                        self.error = AppError("Failed to update tail lines: \(removeResponse.errorOutput)", source: "Log Viewer")
                    }
                } catch {
                    // Rollback local state
                    openLogs[index].tailLines = oldLines
                    self.error = AppError("Failed to update tail lines: \(error.localizedDescription)", source: "Log Viewer")
                }
            }
        }

        // Refresh with new tail count
        Task {
            await fetchSelectedLog()
        }
    }
    
    // MARK: - Pin/Unpin

    /// Pins a temporary log (persists to config via CLI)
    func pinLog(_ id: UUID) {
        guard let index = openLogs.firstIndex(where: { $0.id == id }) else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        let log = openLogs[index]

        Task {
            do {
                // homeboy pin add <project> <path> --type log --tail <lines> --json
                let args = ["pin", "add", projectId, log.path, "--type", "log", "--tail", String(log.tailLines)]
                let response = try await cli.execute(args, timeout: 30)

                if response.success {
                    openLogs[index].isPinned = true
                } else {
                    self.error = AppError("Failed to pin log: \(response.errorOutput)", source: "Log Viewer")
                }
            } catch {
                self.error = AppError("Failed to pin log: \(error.localizedDescription)", source: "Log Viewer")
            }
        }
    }

    /// Unpins a log (removes from config via CLI, tab stays open as temporary)
    func unpinLog(_ id: UUID) {
        guard let index = openLogs.firstIndex(where: { $0.id == id }) else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        let log = openLogs[index]

        Task {
            do {
                // homeboy pin remove <project> <path> --type log --json
                let args = ["pin", "remove", projectId, log.path, "--type", "log"]
                let response = try await cli.execute(args, timeout: 30)

                if response.success {
                    openLogs[index].isPinned = false
                } else {
                    self.error = AppError("Failed to unpin log: \(response.errorOutput)", source: "Log Viewer")
                }
            } catch {
                self.error = AppError("Failed to unpin log: \(error.localizedDescription)", source: "Log Viewer")
            }
        }
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
    
}
