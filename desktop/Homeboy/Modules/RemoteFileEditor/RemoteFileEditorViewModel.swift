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
    
    var displayName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }
    
    var hasUnsavedChanges: Bool {
        content != originalContent
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
class RemoteFileEditorViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    
    // MARK: - Published State
    
    @Published var openFiles: [OpenFile] = []
    @Published var selectedFileId: UUID?
    @Published var isLoading: Bool = false
    @Published var isSaving: Bool = false
    @Published var error: AppError?
    @Published var showFileBrowser: Bool = false
    
    // Confirmation dialogs
    @Published var showSaveConfirmation: Bool = false
    @Published var showCloseConfirmation: Bool = false
    @Published var showDiscardChangesAlert: Bool = false
    @Published var pendingCloseFileId: UUID?
    
    // MARK: - Services
    
    private var sshService: SSHService?
    private var basePath: String?
    
    // MARK: - Computed Properties
    
    var selectedFile: OpenFile? {
        guard let id = selectedFileId else { return nil }
        return openFiles.first { $0.id == id }
    }
    
    var selectedFileIndex: Int? {
        guard let id = selectedFileId else { return nil }
        return openFiles.firstIndex { $0.id == id }
    }
    
    var serverId: String? {
        ConfigurationManager.shared.safeActiveProject.serverId
    }
    
    // MARK: - Initialization
    
    init() {
        setupSSH()
        loadPinnedFiles()
        setupSiteChangeObserver()
    }
    
    private func setupSSH() {
        sshService = SSHService()
        basePath = ConfigurationManager.shared.safeActiveProject.basePath
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
        let fullPath = "\(base)/\(file.path)"
        
        // Check if file exists
        let checkCommand = "test -f '\(fullPath)' && echo 'EXISTS' || echo 'NOTFOUND'"
        
        do {
            let checkResult = try await ssh.executeCommandSync(checkCommand)
            let exists = checkResult.trimmingCharacters(in: .whitespacesAndNewlines) == "EXISTS"
            
            openFiles[index].fileExists = exists
            
            if exists {
                // Fetch content
                let catCommand = "cat '\(fullPath)'"
                let output = try await ssh.executeCommandSync(catCommand)
                openFiles[index].content = output
                openFiles[index].originalContent = output
                openFiles[index].lastFetched = Date()
            } else {
                openFiles[index].content = ""
                openFiles[index].originalContent = ""
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
        let fullPath = "\(base)/\(file.path)"
        
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
        
        ConfigurationManager.shared.activeProject?.remoteFiles.pinnedFiles = pinnedFiles
        ConfigurationManager.shared.saveActiveProject()
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
    
    // MARK: - Site Switching
    
    private func setupSiteChangeObserver() {
        NotificationCenter.default.publisher(for: .projectDidChange)
            .sink { [weak self] _ in
                self?.resetForSiteSwitch()
            }
            .store(in: &cancellables)
    }
    
    private func resetForSiteSwitch() {
        openFiles = []
        selectedFileId = nil
        error = nil
        
        setupSSH()
        loadPinnedFiles()
        
        // Fetch first file
        if selectedFileId != nil {
            Task {
                await fetchSelectedFile()
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
