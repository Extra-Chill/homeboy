import AppKit
import Combine
import Foundation
import SwiftUI

@MainActor
class ConfigEditorViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    // MARK: - Published State
    
    @Published var selectedFile: ConfigFile = .wpConfig
    @Published var content: String = ""
    @Published var originalContent: String = ""
    @Published var fileExists: Bool = true
    @Published var backups: [ConfigBackup] = []
    @Published var isLoading: Bool = false
    @Published var isSaving: Bool = false
    @Published var error: String?
    @Published var lastFetched: Date?
    
    // Confirmation dialogs
    @Published var showSaveConfirmation: Bool = false
    @Published var showRollbackConfirmation: Bool = false
    @Published var showDiscardChangesAlert: Bool = false
    @Published var showCreateFileConfirmation: Bool = false
    @Published var selectedBackup: ConfigBackup?
    
    // Pending file switch (when user tries to switch with unsaved changes)
    private var pendingFileSwitch: ConfigFile?
    
    // MARK: - Services
    
    private var sshService: SSHService?
    private var appPath: String?   // WordPress root (parent of wp-content)
    private let backupService = BackupService.shared
    
    // MARK: - Computed Properties
    
    var hasUnsavedChanges: Bool {
        content != originalContent
    }
    
    var lastFetchedFormatted: String {
        guard let date = lastFetched else { return "Never" }
        
        let secondsAgo = Date().timeIntervalSince(date)
        if secondsAgo < 5 {
            return "Just now"
        }
        
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: Date())
    }
    
    // MARK: - Initialization
    
    init() {
        setupSSH()
    }
    
    private func setupSSH() {
        sshService = SSHService()
        
        // Derive appPath from wp-content path (parent directory)
        let project = ConfigurationManager.readCurrentProject()
        if let wpContentPath = project.wordpress?.wpContentPath, !wpContentPath.isEmpty {
            // appPath is the parent of wp-content
            appPath = (wpContentPath as NSString).deletingLastPathComponent
        } else {
            appPath = nil
        }
    }
    
    // MARK: - File Operations
    
    /// Fetches the selected file from the server
    func fetchFile() async {
        guard let ssh = sshService, let appDir = appPath, !appDir.isEmpty else {
            error = "SSH or WordPress not configured. Check Settings."
            return
        }
        
        isLoading = true
        error = nil
        
        let path = selectedFile.remotePath(appPath: appDir)
        
        // Check if file exists
        let checkCommand = "test -f '\(path)' && echo 'EXISTS' || echo 'NOTFOUND'"
        
        do {
            let checkResult = try await ssh.executeCommandSync(checkCommand)
            let exists = checkResult.trimmingCharacters(in: .whitespacesAndNewlines) == "EXISTS"
            fileExists = exists
            
            if exists {
                // Fetch content
                let catCommand = "cat '\(path)'"
                let output = try await ssh.executeCommandSync(catCommand)
                content = output
                originalContent = output
                lastFetched = Date()
            } else {
                content = ""
                originalContent = ""
            }
            
            // Refresh backup list
            refreshBackups()
            
        } catch {
            self.error = error.localizedDescription
        }
        
        isLoading = false
    }
    
    /// Saves the current content to the server (called after confirmation)
    func saveFile() async {
        guard let ssh = sshService, let appDir = appPath, !appDir.isEmpty else {
            error = "SSH or WordPress not configured. Check Settings."
            return
        }
        
        isSaving = true
        error = nil
        
        let path = selectedFile.remotePath(appPath: appDir)
        
        do {
            // First, backup the current server content locally
            if fileExists {
                let currentServerContent = try await ssh.executeCommandSync("cat '\(path)'")
                backupService.saveBackup(file: selectedFile, content: currentServerContent)
            }
            
            // Write new content using heredoc with quoted delimiter to prevent shell expansion
            // Using a unique delimiter to avoid conflicts with file content
            let writeCommand = "cat > '\(path)' << 'CONFIGEDITOREOF'\n\(content)\nCONFIGEDITOREOF"
            _ = try await ssh.executeCommandSync(writeCommand)
            
            // Update state
            originalContent = content
            fileExists = true
            lastFetched = Date()
            
            // Refresh backups
            refreshBackups()
            
        } catch {
            self.error = "Failed to save: \(error.localizedDescription)"
        }
        
        isSaving = false
    }
    
    /// Creates a new file with default template
    func createFile() async {
        guard let ssh = sshService, let appDir = appPath, !appDir.isEmpty else {
            error = "SSH or WordPress not configured. Check Settings."
            return
        }
        
        guard selectedFile.canCreate else {
            error = "This file cannot be created."
            return
        }
        
        isSaving = true
        error = nil
        
        let path = selectedFile.remotePath(appPath: appDir)
        let template = selectedFile.defaultTemplate
        
        do {
            let writeCommand = "cat > '\(path)' << 'CONFIGEDITOREOF'\n\(template)\nCONFIGEDITOREOF"
            _ = try await ssh.executeCommandSync(writeCommand)
            
            // Update state
            content = template
            originalContent = template
            fileExists = true
            lastFetched = Date()
            
        } catch {
            self.error = "Failed to create file: \(error.localizedDescription)"
        }
        
        isSaving = false
    }
    
    // MARK: - Tab Switching
    
    /// Attempts to switch to a different file tab
    /// Returns true if switch was immediate, false if confirmation needed
    func selectFile(_ file: ConfigFile) -> Bool {
        guard file != selectedFile else { return true }
        
        if hasUnsavedChanges {
            pendingFileSwitch = file
            showDiscardChangesAlert = true
            return false
        }
        
        performFileSwitch(to: file)
        return true
    }
    
    /// Confirms discarding changes and switches to pending file
    func confirmDiscardChanges() {
        guard let file = pendingFileSwitch else { return }
        performFileSwitch(to: file)
        pendingFileSwitch = nil
    }
    
    /// Cancels the pending file switch
    func cancelDiscardChanges() {
        pendingFileSwitch = nil
    }
    
    private func performFileSwitch(to file: ConfigFile) {
        selectedFile = file
        content = ""
        originalContent = ""
        fileExists = true
        error = nil
        lastFetched = nil
        
        Task {
            await fetchFile()
        }
    }
    
    // MARK: - Backup Operations
    
    /// Refreshes the backup list for the current file
    func refreshBackups() {
        backups = backupService.getBackups(for: selectedFile)
    }
    
    /// Loads a backup into the editor (doesn't save to server)
    func restoreBackup(_ backup: ConfigBackup) {
        guard let restoredContent = backupService.loadBackup(backup) else {
            error = "Failed to load backup"
            return
        }
        
        content = restoredContent
        // Note: originalContent stays the same, so hasUnsavedChanges will be true
        // User can review and then save to push to server
    }
    
    /// Clears all backups for the current file
    func clearBackups() {
        backupService.clearBackups(for: selectedFile)
        refreshBackups()
    }
    
    // MARK: - Utility
    
    /// Copies current content to clipboard
    func copyContent() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(content, forType: .string)
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
        // Clear content and state
        content = ""
        originalContent = ""
        fileExists = true
        error = nil
        lastFetched = nil
        backups = []
        
        // Recreate SSH service with new site config
        setupSSH()
    }
}
