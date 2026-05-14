import AppKit
import Combine
import Foundation
import SwiftUI

@MainActor
class RemoteFileEditorViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    // Reference to the file browser for reconnection
    weak var browser: RemoteFileBrowser?

    // MARK: - Published State

    @Published var openFiles: [OpenFile] = []
    @Published var selectedFileId: UUID?
    @Published var isLoading: Bool = false
    @Published var isSaving: Bool = false
    @Published var error: (any DisplayableError)?

    // Sidebar state (persisted per-extension)
    @AppStorage("fileEditor.sidebarCollapsed") var sidebarCollapsed: Bool = false

    // Confirmation dialogs
    @Published var showSaveConfirmation: Bool = false
    @Published var showCloseConfirmation: Bool = false
    @Published var showDiscardChangesAlert: Bool = false
    @Published var pendingCloseFileId: UUID?

    // MARK: - CLI Bridge

    private let cli = HomeboyCLI.shared

    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

    // MARK: - Computed Properties

    var selectedFile: OpenFile? {
        guard let id = selectedFileId else { return nil }
        return openFiles.first { $0.id == id }
    }

    var selectedFileIndex: Int? {
        guard let id = selectedFileId else { return nil }
        return openFiles.firstIndex { $0.id == id }
    }

    // MARK: - Initialization

    init(browser: RemoteFileBrowser? = nil) {
        self.browser = browser
        loadPinnedFiles()
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            // Full reset on project switch
            openFiles = []
            selectedFileId = nil
            error = nil
            loadPinnedFiles()

            // Reconnect browser to new project
            Task {
                let projectId = ConfigurationManager.shared.safeActiveProject.id
                let basePath = ConfigurationManager.shared.safeActiveProject.basePath
                await browser?.reconnect(projectId: projectId, startingPath: basePath)
            }

            if selectedFileId != nil {
                Task {
                    await fetchSelectedFile()
                }
            }
        case .projectModified(_, let fields):
            // Reload pinned files if remoteFiles changed
            if fields.contains(.remoteFiles) {
                reconcilePinnedFiles()
            }
        default:
            break
        }
    }

    private func loadPinnedFiles() {
        let config = ConfigurationManager.shared.safeActiveProject
        openFiles = config.remoteFiles.pinnedFiles.map { OpenFile(from: $0) }

        // Select first file if available
        if let first = openFiles.first {
            selectedFileId = first.id
        }
    }

    private func reconcilePinnedFiles() {
        let pinnedFiles = ConfigurationManager.shared.safeActiveProject.remoteFiles.pinnedFiles
        var pinnedByPath: [String: PinnedRemoteFile] = [:]
        for pinned in pinnedFiles {
            pinnedByPath[pinned.path] = pinned
        }

        for index in openFiles.indices {
            openFiles[index].isPinned = pinnedByPath[openFiles[index].path] != nil
        }

        for pinned in pinnedFiles where !openFiles.contains(where: { $0.path == pinned.path }) {
            openFiles.append(OpenFile(from: pinned))
        }

        if selectedFileId == nil, let first = openFiles.first {
            selectedFileId = first.id
        }
    }

    // MARK: - File Operations

    /// Fetches the currently selected file from the server via CLI
    func fetchSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        isLoading = true
        defer { isLoading = false }
        error = nil

        let file = openFiles[index]

        do {
            let output = try await cli.fileRead(projectId: projectId, path: file.path)
            guard let currentIndex = openFiles.firstIndex(where: { $0.id == file.id }) else { return }
            if let content = output.content {
                openFiles[currentIndex].content = content
                openFiles[currentIndex].originalContent = content
                openFiles[currentIndex].fileSize = output.size
                openFiles[currentIndex].fileExists = true
                openFiles[currentIndex].lastFetched = Date()
            }
        } catch let cliError as CLIBridgeError {
            guard let currentIndex = openFiles.firstIndex(where: { $0.id == file.id }) else { return }
            if let structuredError = cliError.cliError,
               structuredError.code == "file_not_found" {
                openFiles[currentIndex].fileExists = false
                openFiles[currentIndex].content = ""
                openFiles[currentIndex].originalContent = ""
                openFiles[currentIndex].fileSize = nil
            } else {
                self.error = cliError.cliError ?? AppError(cliError.localizedDescription, source: "File Editor", path: file.displayName)
            }
        } catch {
            self.error = error.toDisplayableError(source: "File Editor", path: file.displayName)
        }
    }

    /// Saves the currently selected file to the server via CLI
    func saveSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        isSaving = true
        defer { isSaving = false }
        error = nil

        let file = openFiles[index]

        do {
            _ = try await cli.fileWrite(projectId: projectId, path: file.path, content: file.content)
            if let currentIndex = openFiles.firstIndex(where: { $0.id == file.id }) {
                openFiles[currentIndex].originalContent = file.content
                openFiles[currentIndex].fileExists = true
                openFiles[currentIndex].lastFetched = Date()
            }
        } catch {
            self.error = AppError("Failed to save: \(error.localizedDescription)", source: "File Editor", path: file.displayName)
        }
    }
    
    // MARK: - Tab Management
    
    /// Selects a file tab
    func selectFile(_ id: UUID) {
        guard let file = openFiles.first(where: { $0.id == id }) else { return }
        
        selectedFileId = id
        
        // Fetch if not loaded yet
        if file.lastFetched == nil {
            Task {
                await fetchSelectedFile()
            }
        }
    }
    
    /// Opens a file from the file browser
    func openFile(path: String) {
        // Check if already open
        if let existing = openFiles.first(where: { $0.path == path }) {
            selectedFileId = existing.id
            return
        }
        
        // Create new temporary tab
        let newFile = OpenFile(path: path, isPinned: false)
        openFiles.append(newFile)
        selectedFileId = newFile.id
        
        Task {
            await fetchSelectedFile()
        }
    }

    func openFileFromBrowser(path: String) {
        openFile(path: relativeFilePath(for: path))
    }
    
    /// Attempts to close a file tab
    func closeFile(_ id: UUID) {
        guard let file = openFiles.first(where: { $0.id == id }) else { return }
        
        if file.hasUnsavedChanges {
            pendingCloseFileId = id
            showCloseConfirmation = true
            return
        }
        
        performClose(id)
    }
    
    /// Confirms closing a file with unsaved changes
    func confirmClose() {
        guard let id = pendingCloseFileId else { return }
        performClose(id)
        pendingCloseFileId = nil
    }
    
    private func performClose(_ id: UUID) {
        guard let index = openFiles.firstIndex(where: { $0.id == id }) else { return }
        
        let file = openFiles[index]
        
        // If pinned, just unpin instead of closing
        if file.isPinned {
            unpinFile(id)
        }
        
        openFiles.remove(at: index)
        
        // Select another tab if needed
        if selectedFileId == id {
            selectedFileId = openFiles.first?.id
        }
    }
    
    // MARK: - Pin/Unpin

    /// Pins a temporary file (persists to config via CLI)
    func pinFile(_ id: UUID) {
        guard let index = openFiles.firstIndex(where: { $0.id == id }) else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        let file = openFiles[index]

        Task {
            do {
                _ = try await cli.filePinAdd(projectId: projectId, path: file.path)
                if let currentIndex = openFiles.firstIndex(where: { $0.id == file.id }) {
                    openFiles[currentIndex].isPinned = true
                }
            } catch {
                self.error = AppError("Failed to pin file: \(error.localizedDescription)", source: "File Editor")
            }
        }
    }

    /// Unpins a file (removes from config via CLI, tab stays open as temporary)
    func unpinFile(_ id: UUID) {
        guard let index = openFiles.firstIndex(where: { $0.id == id }) else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        let file = openFiles[index]

        Task {
            do {
                _ = try await cli.filePinRemove(projectId: projectId, path: file.path)
                if let currentIndex = openFiles.firstIndex(where: { $0.id == file.id }) {
                    openFiles[currentIndex].isPinned = false
                }
            } catch {
                self.error = AppError("Failed to unpin file: \(error.localizedDescription)", source: "File Editor")
            }
        }
    }

    /// Toggles pin state
    func togglePin(_ id: UUID) {
        guard let file = openFiles.first(where: { $0.id == id }) else { return }

        if file.isPinned {
            unpinFile(id)
        } else {
            pinFile(id)
        }
    }
    
    // MARK: - Content Updates
    
    /// Updates the content of the selected file
    func updateContent(_ newContent: String) {
        guard let index = selectedFileIndex else { return }
        openFiles[index].content = newContent
    }
    
    /// Copies current content to clipboard
    func copyContent() {
        guard let file = selectedFile else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(file.content, forType: .string)
    }
    
    // MARK: - Sidebar File Operations

    /// Handle a file being deleted from the sidebar - close its tab if open
    func handleFileDeleted(_ path: String) {
        let relativePath = relativeFilePath(for: path)

        if let file = openFiles.first(where: { $0.path == relativePath }) {
            performClose(file.id)
        }
    }

    /// Handle a file being renamed from the sidebar - update the tab if open
    func handleFileRenamed(from oldPath: String, to newPath: String) {
        let oldRelative = relativeFilePath(for: oldPath)
        let newRelative = relativeFilePath(for: newPath)

        if let index = openFiles.firstIndex(where: { $0.path == oldRelative }) {
            // Update the file path
            let oldFile = openFiles[index]
            openFiles[index] = OpenFile(id: oldFile.id, path: newRelative, isPinned: oldFile.isPinned)
            openFiles[index].content = oldFile.content
            openFiles[index].originalContent = oldFile.originalContent
            openFiles[index].fileExists = oldFile.fileExists
            openFiles[index].lastFetched = oldFile.lastFetched
            openFiles[index].fileSize = oldFile.fileSize

            // Update pinned files via CLI if this was pinned
            if oldFile.isPinned && cli.isInstalled {
                Task {
                    do {
                        _ = try await cli.filePinUpdatePath(projectId: projectId, oldPath: oldRelative, newPath: newRelative)
                    } catch {
                        // Non-critical error - file is still renamed locally
                    }
                }
            }
        }
    }

    private func relativeFilePath(for path: String) -> String {
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
    
    // MARK: - Utility
    
    func lastFetchedFormatted(for file: OpenFile) -> String {
        guard let date = file.lastFetched else { return "Not loaded" }
        
        let secondsAgo = Date().timeIntervalSince(date)
        if secondsAgo < 5 {
            return "Just now"
        }
        
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: Date())
    }
}
