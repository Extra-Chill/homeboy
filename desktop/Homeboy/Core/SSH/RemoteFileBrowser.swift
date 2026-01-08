import Foundation
import SwiftUI

/// Browsing mode for the remote file browser
enum FileBrowserMode {
    case browse              // General file browsing
    case selectPath          // Selecting a directory path (e.g., wp-content picker)
}

/// Observable class for managing remote file system browsing
@MainActor
class RemoteFileBrowser: ObservableObject {
    @Published var currentPath: String = ""
    @Published var entries: [RemoteFileEntry] = []
    @Published var isLoading: Bool = false
    @Published var error: String?
    @Published var pathHistory: [String] = []
    
    private var ssh: SSHService?
    private let serverId: String
    
    /// Initialize with a server ID
    init(serverId: String) {
        self.serverId = serverId
    }
    
    /// Connect to the server and navigate to home directory
    func connect() async {
        guard let server = ConfigurationManager.readServer(id: serverId) else {
            error = "Server not found"
            return
        }
        
        guard let sshService = SSHService(server: server) else {
            error = "Failed to initialize SSH connection"
            return
        }
        
        self.ssh = sshService
        await goToHome()
    }
    
    /// Navigate to the SSH user's home directory
    func goToHome() async {
        guard let ssh = ssh else {
            error = "Not connected"
            return
        }
        
        isLoading = true
        error = nil
        
        do {
            let homePath = try await ssh.getHomeDirectory()
            await goToPath(homePath)
        } catch {
            self.error = error.localizedDescription
            isLoading = false
        }
    }
    
    /// Navigate to a specific path
    func goToPath(_ path: String) async {
        guard let ssh = ssh else {
            error = "Not connected"
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
            self.error = error.localizedDescription
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
}
