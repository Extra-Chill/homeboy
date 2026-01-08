import Foundation

/// Represents a file or directory entry on a remote server
struct RemoteFileEntry: Identifiable, Hashable, Comparable {
    var id: String { path }
    
    let name: String
    let path: String
    let isDirectory: Bool
    let size: Int64?
    let modifiedDate: Date?
    let permissions: String?
    
    /// System icon name for file type
    var icon: String {
        if isDirectory {
            return "folder.fill"
        }
        
        let ext = (name as NSString).pathExtension.lowercased()
        switch ext {
        case "php": return "chevron.left.forwardslash.chevron.right"
        case "js", "ts", "jsx", "tsx": return "curlybraces"
        case "css", "scss", "sass": return "paintbrush.fill"
        case "html", "htm": return "doc.text.fill"
        case "json", "xml", "yaml", "yml": return "doc.badge.gearshape.fill"
        case "md", "txt": return "doc.plaintext.fill"
        case "png", "jpg", "jpeg", "gif", "svg", "webp": return "photo.fill"
        case "zip", "tar", "gz", "rar": return "doc.zipper"
        case "sh", "bash": return "terminal.fill"
        case "sql": return "cylinder.fill"
        default: return "doc.fill"
        }
    }
    
    /// Formatted file size string
    var formattedSize: String? {
        guard let size = size, !isDirectory else { return nil }
        return ByteCountFormatter.string(fromByteCount: size, countStyle: .file)
    }
    
    /// Parent directory path
    var parentPath: String {
        (path as NSString).deletingLastPathComponent
    }
    
    /// Creates entry from ls -la output line
    static func parse(lsLine: String, basePath: String) -> RemoteFileEntry? {
        // Example: drwxr-xr-x  5 user group  160 Jan  8 10:30 dirname
        // Example: -rw-r--r--  1 user group 1234 Jan  8 10:30 filename.txt
        let components = lsLine.split(separator: " ", omittingEmptySubsequences: true)
        guard components.count >= 9 else { return nil }
        
        let permissions = String(components[0])
        let isDirectory = permissions.hasPrefix("d")
        let size = Int64(components[4]) ?? 0
        
        // Name is everything after the 8th component (handles spaces in names)
        let nameStartIndex = lsLine.range(of: String(components[8]))?.lowerBound ?? lsLine.endIndex
        let name = String(lsLine[nameStartIndex...]).trimmingCharacters(in: .whitespaces)
        
        // Skip . and ..
        guard name != "." && name != ".." else { return nil }
        
        // Handle symlinks (name -> target)
        let actualName = name.components(separatedBy: " -> ").first ?? name
        
        let path = basePath.hasSuffix("/") ? "\(basePath)\(actualName)" : "\(basePath)/\(actualName)"
        
        return RemoteFileEntry(
            name: actualName,
            path: path,
            isDirectory: isDirectory || permissions.hasPrefix("l"), // Treat symlinks as directories for navigation
            size: isDirectory ? nil : size,
            modifiedDate: nil,
            permissions: permissions
        )
    }
    
    // MARK: - Comparable (directories first, then alphabetical by name)
    
    static func < (lhs: RemoteFileEntry, rhs: RemoteFileEntry) -> Bool {
        // Directories always come before files
        if lhs.isDirectory != rhs.isDirectory {
            return lhs.isDirectory
        }
        // Same type: sort alphabetically by name (case-insensitive)
        return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
    }
}
