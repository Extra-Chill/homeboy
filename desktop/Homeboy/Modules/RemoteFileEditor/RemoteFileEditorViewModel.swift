import AppKit
import Combine
import Foundation
import SwiftUI

/// Represents an open file tab in the Remote File Editor
struct OpenFile: PinnableTabItem, Equatable {
    let id: UUID
    let path: String           // Relative path from basePath
    var isPinned: Bool
    var content: String = ""
    var originalContent: String = ""
    var fileExists: Bool = true
    var lastFetched: Date?
    var fileSize: Int64?       // Size in bytes

    var displayName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }

    var hasUnsavedChanges: Bool {
        content != originalContent
    }

    var formattedSize: String {
        guard let size = fileSize else { return "" }
        return ByteCountFormatter.string(fromByteCount: size, countStyle: .file)
    }

    init(id: UUID = UUID(), path: String, isPinned: Bool) {
        self.id = id
        self.path = path
        self.isPinned = isPinned
    }

    init(from pinned: PinnedRemoteFile) {
        self.id = pinned.id
        self.path = pinned.path
        self.isPinned = true
    }
}

// MARK: - CLI Response Types

private struct CLIResponse<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
}

/// Response from `file read --json`
private struct FileReadResponse: Decodable {
    let path: String
    let size: Int64?
    let content: String
}

/// Response from `file write --json`
private struct FileWriteResponse: Decodable {
    let path: String
    let bytesWritten: Int
}

@MainActor
class RemoteFileEditorViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    // MARK: - Published State

    @Published var openFiles: [OpenFile] = []
    @Published var selectedFileId: UUID?
    @Published var isLoading: Bool = false
    @Published var isSaving: Bool = false
    @Published var error: AppError?

    // Sidebar state (persisted per-module)
    @AppStorage("fileEditor.sidebarCollapsed") var sidebarCollapsed: Bool = false

    // Confirmation dialogs
    @Published var showSaveConfirmation: Bool = false
    @Published var showCloseConfirmation: Bool = false
    @Published var showDiscardChangesAlert: Bool = false
    @Published var pendingCloseFileId: UUID?

    // MARK: - CLI Bridge

    private let cli = CLIBridge.shared

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

    init() {
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
            if selectedFileId != nil {
                Task {
                    await fetchSelectedFile()
                }
            }
        case .projectModified(_, let fields):
            // Reload pinned files if remoteFiles changed
            if fields.contains(.remoteFiles) {
                loadPinnedFiles()
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

    // MARK: - File Operations

    /// Fetches the currently selected file from the server via CLI
    func fetchSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        isLoading = true
        error = nil

        let file = openFiles[index]

        do {
            // homeboy file read <project> <path> --json
            let args = ["file", "read", projectId, file.path]
            let response = try await cli.execute(args, timeout: 60)

            if response.success {
                // Parse JSON response through CLIResponse wrapper
                if let data = response.output.data(using: .utf8),
                   let wrapper = try? JSONDecoder().decode(CLIResponse<FileReadResponse>.self, from: data),
                   let fileContent = wrapper.data {
                    openFiles[index].content = fileContent.content
                    openFiles[index].originalContent = fileContent.content
                    openFiles[index].fileSize = fileContent.size
                    openFiles[index].fileExists = true
                    openFiles[index].lastFetched = Date()
                }
            } else {
                // Check if error indicates file not found
                if response.errorOutput.contains("not found") || response.errorOutput.contains("No such file") {
                    openFiles[index].fileExists = false
                    openFiles[index].content = ""
                    openFiles[index].originalContent = ""
                    openFiles[index].fileSize = nil
                } else {
                    self.error = AppError(response.errorOutput, source: "File Editor", path: file.displayName)
                }
            }
        } catch {
            self.error = AppError(error.localizedDescription, source: "File Editor", path: file.displayName)
        }

        isLoading = false
    }

    /// Saves the currently selected file to the server via CLI
    func saveSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings → CLI.", source: "File Editor")
            return
        }

        isSaving = true
        error = nil

        let file = openFiles[index]

        do {
            // homeboy file write <project> <path> --json (content via stdin)
            let args = ["file", "write", projectId, file.path]
            let response = try await cli.executeWithStdin(args, stdin: file.content, timeout: 60)

            if response.success {
                // Update state
                openFiles[index].originalContent = file.content
                openFiles[index].fileExists = true
                openFiles[index].lastFetched = Date()
            } else {
                self.error = AppError("Failed to save: \(response.errorOutput)", source: "File Editor", path: file.displayName)
            }
        } catch {
            self.error = AppError("Failed to save: \(error.localizedDescription)", source: "File Editor", path: file.displayName)
        }

        isSaving = false
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
                // homeboy pin add <project> <path> --type file --json
                let args = ["pin", "add", projectId, file.path, "--type", "file"]
                let response = try await cli.execute(args, timeout: 30)

                if response.success {
                    openFiles[index].isPinned = true
                } else {
                    self.error = AppError("Failed to pin file: \(response.errorOutput)", source: "File Editor")
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
                // homeboy pin remove <project> <path> --type file --json
                let args = ["pin", "remove", projectId, file.path, "--type", "file"]
                let response = try await cli.execute(args, timeout: 30)

                if response.success {
                    openFiles[index].isPinned = false
                } else {
                    self.error = AppError("Failed to unpin file: \(response.errorOutput)", source: "File Editor")
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
        // Convert absolute path to relative if needed
        let basePath = ConfigurationManager.shared.safeActiveProject.basePath
        let relativePath: String
        if let base = basePath, path.hasPrefix(base) {
            relativePath = String(path.dropFirst(base.count + 1))
        } else {
            relativePath = path
        }

        if let file = openFiles.first(where: { $0.path == relativePath }) {
            performClose(file.id)
        }
    }

    /// Handle a file being renamed from the sidebar - update the tab if open
    func handleFileRenamed(from oldPath: String, to newPath: String) {
        // Convert absolute paths to relative if needed
        let basePath = ConfigurationManager.shared.safeActiveProject.basePath
        let oldRelative: String
        let newRelative: String

        if let base = basePath {
            oldRelative = oldPath.hasPrefix(base) ? String(oldPath.dropFirst(base.count + 1)) : oldPath
            newRelative = newPath.hasPrefix(base) ? String(newPath.dropFirst(base.count + 1)) : newPath
        } else {
            oldRelative = oldPath
            newRelative = newPath
        }

        if let index = openFiles.firstIndex(where: { $0.path == oldRelative }) {
            // Update the file path
            let oldFile = openFiles[index]
            openFiles[index] = OpenFile(id: oldFile.id, path: newRelative, isPinned: oldFile.isPinned)
            openFiles[index].content = oldFile.content
            openFiles[index].originalContent = oldFile.originalContent
            openFiles[index].fileExists = oldFile.fileExists
            openFiles[index].lastFetched = oldFile.lastFetched

            // Update pinned files via CLI if this was pinned
            if oldFile.isPinned && cli.isInstalled {
                Task {
                    do {
                        // Remove old pin
                        let removeArgs = ["pin", "remove", projectId, oldRelative, "--type", "file"]
                        let removeResponse = try await cli.execute(removeArgs, timeout: 30)

                        if removeResponse.success {
                            // Add new pin
                            let addArgs = ["pin", "add", projectId, newRelative, "--type", "file"]
                            _ = try await cli.execute(addArgs, timeout: 30)
                        }
                    } catch {
                        // Non-critical error - file is still renamed locally
                    }
                }
            }
        }
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
