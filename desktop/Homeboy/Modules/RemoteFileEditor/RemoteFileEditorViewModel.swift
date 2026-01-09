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
    
    // MARK: - Services

    private var sshService: SSHService?
    private var basePath: String?
    private var pathResolver: RemotePathResolver?
    
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
        setupSSH()
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
            setupSSH()
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
            // Reconnect SSH if server or basePath changed
            if fields.contains(.server) || fields.contains(.basePath) {
                setupSSH()
            }
        default:
            break
        }
    }
    
    private func setupSSH() {
        let project = ConfigurationManager.shared.safeActiveProject
        sshService = SSHService()
        basePath = project.basePath
        pathResolver = RemotePathResolver(project: project)
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
    
    /// Fetches the currently selected file from the server
    func fetchSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard let ssh = sshService, let base = basePath, !base.isEmpty else {
            error = AppError("SSH or base path not configured. Check Settings.", source: "File Editor")
            return
        }
        
        isLoading = true
        error = nil
        
        let file = openFiles[index]
        let fullPath = pathResolver?.filePath(file.path) ?? RemotePathResolver.join(base, file.path)

        // Check if file exists
        let checkCommand = "test -f '\(fullPath)' && echo 'EXISTS' || echo 'NOTFOUND'"
        
        do {
            let checkResult = try await ssh.executeCommandSync(checkCommand)
            let exists = checkResult.trimmingCharacters(in: .whitespacesAndNewlines) == "EXISTS"
            
            openFiles[index].fileExists = exists
            
            if exists {
                // Get file size (macOS uses -f%z, Linux uses --printf='%s')
                let sizeCommand = "stat -f%z '\(fullPath)' 2>/dev/null || stat --printf='%s' '\(fullPath)' 2>/dev/null"
                if let sizeStr = try? await ssh.executeCommandSync(sizeCommand),
                   let size = Int64(sizeStr.trimmingCharacters(in: .whitespacesAndNewlines)) {
                    openFiles[index].fileSize = size
                }

                // Fetch content
                let catCommand = "cat '\(fullPath)'"
                let output = try await ssh.executeCommandSync(catCommand)
                openFiles[index].content = output
                openFiles[index].originalContent = output
                openFiles[index].lastFetched = Date()
            } else {
                openFiles[index].content = ""
                openFiles[index].originalContent = ""
                openFiles[index].fileSize = nil
            }
        } catch {
            self.error = AppError(error.localizedDescription, source: "File Editor", path: file.displayName)
        }
        
        isLoading = false
    }
    
    /// Saves the currently selected file to the server
    func saveSelectedFile() async {
        guard let index = selectedFileIndex else { return }
        guard let ssh = sshService, let base = basePath, !base.isEmpty else {
            error = AppError("SSH or base path not configured. Check Settings.", source: "File Editor")
            return
        }
        
        isSaving = true
        error = nil
        
        let file = openFiles[index]
        let fullPath = pathResolver?.filePath(file.path) ?? RemotePathResolver.join(base, file.path)

        do {
            // Write content using heredoc
            let writeCommand = "cat > '\(fullPath)' << 'FILEEDITOREOF'\n\(file.content)\nFILEEDITOREOF"
            _ = try await ssh.executeCommandSync(writeCommand)
            
            // Update state
            openFiles[index].originalContent = file.content
            openFiles[index].fileExists = true
            openFiles[index].lastFetched = Date()
            
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
    
    /// Pins a temporary file (persists to config)
    func pinFile(_ id: UUID) {
        guard let index = openFiles.firstIndex(where: { $0.id == id }) else { return }
        
        openFiles[index].isPinned = true
        savePinnedFiles()
    }
    
    /// Unpins a file (removes from config, tab stays open as temporary)
    func unpinFile(_ id: UUID) {
        guard let index = openFiles.firstIndex(where: { $0.id == id }) else { return }
        
        openFiles[index].isPinned = false
        savePinnedFiles()
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
    
    private func savePinnedFiles() {
        let pinnedFiles = openFiles
            .filter { $0.isPinned }
            .map { PinnedRemoteFile(id: $0.id, path: $0.path) }
        
        ConfigurationManager.shared.updateActiveProject { $0.remoteFiles.pinnedFiles = pinnedFiles }
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
            
            // Update pinned files if this was pinned
            if oldFile.isPinned {
                savePinnedFiles()
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
