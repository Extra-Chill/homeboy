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

    private let projectId: String
    private let startingPath: String?
    private let cli = HomeboyCLI.shared

    /// Initialize with a project ID and optional starting path
    init(projectId: String, startingPath: String? = nil) {
        self.projectId = projectId
        self.startingPath = startingPath
    }

    /// Connect and navigate to starting path or root
    func connect() async {
        if let startingPath = startingPath, !startingPath.isEmpty {
            await goToPath(startingPath)
        } else {
            await goToPath("/")
        }
    }

    /// Navigate to the remote root directory
    func goToHome() async {
        await goToPath("/")
    }

    /// Navigate to a specific path
    func goToPath(_ path: String) async {
        isLoading = true
        error = nil

        do {
            let output = try await cli.fileList(projectId: projectId, path: path)
            currentPath = path
            entries = (output.entries ?? []).map { entry in
                RemoteFileEntry(
                    name: entry.name,
                    path: entry.path,
                    isDirectory: entry.isDirectory,
                    size: entry.size,
                    modifiedDate: nil,
                    permissions: entry.permissions
                )
            }

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
        let parentPath = RemotePathResolver.parent(of: currentPath)
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
        _ = try await cli.fileDelete(projectId: projectId, path: entry.path, recursive: entry.isDirectory)
        await refresh()
    }

    /// Rename a file or directory
    /// - Parameters:
    ///   - entry: The file or directory to rename
    ///   - newName: The new name (not full path)
    /// - Returns: The new full path
    @discardableResult
    func renameEntry(_ entry: RemoteFileEntry, newName: String) async throws -> String {
        let newPath = RemotePathResolver.join(entry.parentPath, newName)
        _ = try await cli.fileRename(projectId: projectId, oldPath: entry.path, newPath: newPath)
        await refresh()
        return newPath
    }

    /// Create a new empty file in the current directory
    /// - Parameter name: The filename
    /// - Returns: The full path of the created file
    @discardableResult
    func createFile(named name: String) async throws -> String {
        let path = RemotePathResolver.join(currentPath, name)
        _ = try await cli.fileWrite(projectId: projectId, path: path, content: "")
        await refresh()
        return path
    }

    /// Create a new directory in the current directory.
    ///
    /// Not currently supported: the Homeboy CLI does not expose a mkdir-style command.
    /// UI should disable folder creation until the CLI supports it.
    @discardableResult
    func createDirectory(named name: String) async throws -> String {
        throw NSError(
            domain: "Homeboy.RemoteFileBrowser",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "Directory creation is not supported by the Homeboy CLI"]
        )
    }
}
