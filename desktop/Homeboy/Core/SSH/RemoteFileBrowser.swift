import Foundation
import SwiftUI

/// Browsing mode for the remote file browser
enum FileBrowserMode {
    case browse              // General file browsing
    case selectPath          // Selecting a directory path (e.g., wp-content picker)
    case selectFile          // Selecting a file (e.g., Remote File Editor)
}

/// Observable class for managing remote file system browsing
@MainActor
class RemoteFileBrowser: ObservableObject {
    @Published var currentPath: String = ""
    @Published var entries: [RemoteFileEntry] = []
    @Published var isLoading: Bool = false
    @Published var error: AppError?
    @Published var pathHistory: [String] = []
    @Published var selectedEntries: Set<String> = []
    
    /// First selected entry (for single-selection operations)
    var selectedFile: RemoteFileEntry? {
        guard selectedEntries.count == 1,
              let id = selectedEntries.first else { return nil }
        return entries.first { $0.id == id }
    }
    
    /// All selected entries
    var selectedFiles: [RemoteFileEntry] {
        entries.filter { selectedEntries.contains($0.id) }
    }
    
    private var ssh: SSHService?
    private let serverId: String
    private let startingPath: String?
    
    /// Initialize with a server ID and optional starting path
    init(serverId: String, startingPath: String? = nil) {
        self.serverId = serverId
        self.startingPath = startingPath
    }
    
    /// Connect to the server and navigate to starting path or home directory
    func connect() async {
        guard let server = ConfigurationManager.readServer(id: serverId) else {
            error = AppError("Server not found", source: "Remote File Browser")
            return
        }
        
        guard let sshService = SSHService(server: server) else {
            error = AppError("Failed to initialize SSH connection", source: "Remote File Browser")
            return
        }
        
        self.ssh = sshService
        
        if let startingPath = startingPath, !startingPath.isEmpty {
            await goToPath(startingPath)
        } else {
            await goToHome()
        }
    }
    
    /// Navigate to the SSH user's home directory
    func goToHome() async {
        guard let ssh = ssh else {
            error = AppError("Not connected", source: "Remote File Browser")
            return
        }
        
        isLoading = true
        error = nil
        
        do {
            let homePath = try await ssh.getHomeDirectory()
            await goToPath(homePath)
        } catch {
            self.error = AppError(error.localizedDescription, source: "Remote File Browser")
            isLoading = false
        }
    }
    
    /// Navigate to a specific path
    func goToPath(_ path: String) async {
        guard let ssh = ssh else {
            error = AppError("Not connected", source: "Remote File Browser")
            return
        }
        
        isLoading = true
        error = nil
        
        do {
            let newEntries = try await ssh.listDirectory(path)
            currentPath = path
            entries = newEntries
            
            // Add to history if not already the last item
            if pathHistory.last != path {
                pathHistory.append(path)
            }
        } catch {
            self.error = AppError(error.localizedDescription, source: "Remote File Browser", path: path)
        }
        
        isLoading = false
    }
    
    /// Navigate to parent directory
    func goUp() async {
        let parentPath = (currentPath as NSString).deletingLastPathComponent
        guard !parentPath.isEmpty, parentPath != currentPath else { return }
        await goToPath(parentPath)
    }
    
    /// Navigate back in history
    func goBack() async {
        guard pathHistory.count > 1 else { return }
        pathHistory.removeLast() // Remove current
        if let previousPath = pathHistory.last {
            await goToPath(previousPath)
        }
    }
    
    /// Refresh current directory
    func refresh() async {
        await goToPath(currentPath)
    }
    
    /// Navigate into a directory entry
    func navigateInto(_ entry: RemoteFileEntry) async {
        guard entry.isDirectory else { return }
        await goToPath(entry.path)
    }
    
    /// Current path as breadcrumb components
    var breadcrumbs: [(name: String, path: String)] {
        var components: [(name: String, path: String)] = []
        var path = ""
        
        for component in currentPath.split(separator: "/") {
            path += "/\(component)"
            components.append((String(component), path))
        }
        
        return components
    }
    
    /// Whether we can navigate up
    var canGoUp: Bool {
        !currentPath.isEmpty && currentPath != "/"
    }
    
    /// Whether we can navigate back in history
    var canGoBack: Bool {
        pathHistory.count > 1
    }
    
    // MARK: - File Operations
    
    /// Delete a file or directory (refreshes current directory after)
    /// - Parameter entry: The file or directory to delete
    func deleteEntry(_ entry: RemoteFileEntry) async throws {
        guard let ssh = ssh else {
            throw SSHError.noCredentials
        }
        try await ssh.deleteFile(entry.path, recursive: entry.isDirectory)
        await refresh()
    }
    
    /// Rename a file or directory
    /// - Parameters:
    ///   - entry: The file or directory to rename
    ///   - newName: The new name (not full path)
    /// - Returns: The new full path
    @discardableResult
    func renameEntry(_ entry: RemoteFileEntry, newName: String) async throws -> String {
        guard let ssh = ssh else {
            throw SSHError.noCredentials
        }
        let newPath = "\(entry.parentPath)/\(newName)"
        try await ssh.renameFile(from: entry.path, to: newPath)
        await refresh()
        return newPath
    }
    
    /// Create a new empty file in the current directory
    /// - Parameter name: The filename
    /// - Returns: The full path of the created file
    @discardableResult
    func createFile(named name: String) async throws -> String {
        guard let ssh = ssh else {
            throw SSHError.noCredentials
        }
        let path = currentPath.hasSuffix("/") ? "\(currentPath)\(name)" : "\(currentPath)/\(name)"
        try await ssh.createFile(path)
        await refresh()
        return path
    }
    
    /// Create a new directory in the current directory
    /// - Parameter name: The directory name
    /// - Returns: The full path of the created directory
    @discardableResult
    func createDirectory(named name: String) async throws -> String {
        guard let ssh = ssh else {
            throw SSHError.noCredentials
        }
        let path = currentPath.hasSuffix("/") ? "\(currentPath)\(name)" : "\(currentPath)/\(name)"
        try await ssh.createDirectory(path)
        await refresh()
        return path
    }
}
