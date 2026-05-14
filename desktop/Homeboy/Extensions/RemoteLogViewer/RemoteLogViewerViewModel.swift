import AppKit
import Combine
import Foundation

@MainActor
class RemoteLogViewerViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    // MARK: - Published State

    @Published var openLogs: [OpenLog] = []
    @Published var selectedLogId: UUID?
    @Published var isLoading: Bool = false
    @Published var error: (any DisplayableError)?
    @Published var showClearConfirmation: Bool = false

    // MARK: - CLI Bridge

    private let cli = HomeboyCLI.shared

    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

    // MARK: - Tail Options

    static let tailOptions = [50, 100, 500, 1000]

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
            Task { await fetchSelectedLog() }
        case .projectModified(_, let fields):
            // Reload pinned logs if remoteLogs changed
            if fields.contains(.remoteLogs) {
                reconcilePinnedLogs()
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

    private func reconcilePinnedLogs() {
        let pinnedLogs = ConfigurationManager.shared.safeActiveProject.remoteLogs.pinnedLogs
        var pinnedByPath: [String: PinnedRemoteLog] = [:]
        for pinned in pinnedLogs {
            pinnedByPath[pinned.path] = pinned
        }

        for index in openLogs.indices {
            if let pinned = pinnedByPath[openLogs[index].path] {
                openLogs[index].isPinned = true
                openLogs[index].tailLines = max(1, pinned.tailLines)
            } else {
                openLogs[index].isPinned = false
            }
        }

        for pinned in pinnedLogs where !openLogs.contains(where: { $0.path == pinned.path }) {
            openLogs.append(OpenLog(from: pinned))
        }

        if selectedLogId == nil, let first = openLogs.first {
            selectedLogId = first.id
        }
    }

    // MARK: - Log Operations

    /// Fetches the currently selected log from the server via CLI
    func fetchSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard HomeboyCLI.shared.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        isLoading = true
        defer { isLoading = false }
        error = nil

        let log = openLogs[index]

        do {
            let output = try await cli.logsShow(
                projectId: projectId,
                path: log.path,
                lines: log.tailLines
            )

            guard let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) else { return }
            if let logData = output.log {
                openLogs[currentIndex].content = logData.content
                openLogs[currentIndex].fileExists = true
                openLogs[currentIndex].lastFetched = Date()
            } else {
                openLogs[currentIndex].fileExists = false
                openLogs[currentIndex].content = ""
            }
        } catch let cliError as CLIBridgeError {
            guard let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) else { return }
            if let structuredError = cliError.cliError,
               structuredError.code == "file_not_found" {
                openLogs[currentIndex].fileExists = false
                openLogs[currentIndex].content = ""
            } else {
                self.error = cliError.cliError ?? AppError(cliError.localizedDescription, source: "Log Viewer", path: log.displayName)
            }
        } catch {
            self.error = error.toDisplayableError(source: "Log Viewer", path: log.displayName)
        }

    }

    /// Clears the currently selected log file via CLI
    func clearSelectedLog() async {
        guard let index = selectedLogIndex else { return }
        guard HomeboyCLI.shared.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "Log Viewer")
            return
        }

        isLoading = true
        defer { isLoading = false }
        error = nil

        let log = openLogs[index]

        do {
            _ = try await cli.logsClear(projectId: projectId, path: log.path)
            guard let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) else { return }
            openLogs[currentIndex].content = ""
            openLogs[currentIndex].lastFetched = Date()
        } catch {
            self.error = error.toDisplayableError(source: "Log Viewer", path: log.displayName)
        }

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

        if log.isPinned {
            Task {
                do {
                    try await unpinLog(path: log.path)
                } catch {
                    self.error = AppError("Failed to unpin log: \(error.localizedDescription)", source: "Log Viewer")
                }
            }
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
        let tailLines = max(1, lines)
        let oldLines = openLogs[index].tailLines
        openLogs[index].tailLines = tailLines

        // If pinned, update config via CLI (remove and re-add with new tail lines)
        if openLogs[index].isPinned && cli.isInstalled {
            let log = openLogs[index]
            Task {
                do {
                    _ = try await cli.logPinUpdateTail(
                        projectId: projectId,
                        path: log.path,
                        tailLines: tailLines,
                        previousTailLines: oldLines
                    )
                } catch {
                    if let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) {
                        openLogs[currentIndex].tailLines = oldLines
                    }
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
                _ = try await cli.logPinAdd(projectId: projectId, path: log.path, tailLines: log.tailLines)
                if let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) {
                    openLogs[currentIndex].isPinned = true
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
                _ = try await unpinLog(path: log.path)
                if let currentIndex = openLogs.firstIndex(where: { $0.id == log.id }) {
                    openLogs[currentIndex].isPinned = false
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

    func openLogFromBrowser(path: String) {
        openLog(path: relativeLogPath(for: path))
    }

    private func unpinLog(path: String) async throws {
        _ = try await cli.logPinRemove(projectId: projectId, path: path)
    }

    private func relativeLogPath(for path: String) -> String {
        guard path.hasPrefix("/") else { return path }

        guard let basePath = ConfigurationManager.shared.safeActiveProject.basePath,
              !basePath.isEmpty else {
            return path
        }

        let normalizedBase = URL(fileURLWithPath: basePath).standardizedFileURL.path
        let normalizedPath = URL(fileURLWithPath: path).standardizedFileURL.path
        guard normalizedPath == normalizedBase || normalizedPath.hasPrefix(normalizedBase + "/") else {
            return path
        }

        if normalizedPath == normalizedBase {
            return URL(fileURLWithPath: normalizedPath).lastPathComponent
        }

        return String(normalizedPath.dropFirst(normalizedBase.count + 1))
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
