import Foundation

/// Represents a local backup of a remote file
struct FileBackup: Identifiable {
    let id: String
    let filePath: String
    let date: Date
    let url: URL
    
    var displayDate: String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
    
    var fileName: String {
        URL(fileURLWithPath: filePath).lastPathComponent
    }
}

/// Manages local backups of server files.
/// Backups are stored in ~/Library/Application Support/Homeboy/backups/
class BackupService {
    static let shared = BackupService()
    
    private let fileManager = FileManager.default
    private let maxBackupsPerFile = 10
    
    /// Base directory for backups
    private var backupsDirectory: URL {
        AppPaths.backups
    }
    
    private init() {
        try? fileManager.createDirectory(at: backupsDirectory, withIntermediateDirectories: true)
    }
    
    /// Creates a safe directory name from a file path
    private func safeDirectoryName(for path: String) -> String {
        // Replace path separators and other unsafe characters
        path.replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: ":", with: "_")
            .replacingOccurrences(of: " ", with: "_")
    }
    
    /// Directory for a specific file's backups
    private func directory(for path: String) -> URL {
        let safeName = safeDirectoryName(for: path)
        let dir = backupsDirectory.appendingPathComponent(safeName)
        try? fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
    
    /// Saves a backup of the current server content before overwriting
    @discardableResult
    func saveBackup(filePath: String, content: String) -> FileBackup? {
        let timestamp = ISO8601DateFormatter().string(from: Date())
            .replacingOccurrences(of: ":", with: "-")
        let filename = "\(timestamp).txt"
        let url = directory(for: filePath).appendingPathComponent(filename)
        
        do {
            try content.write(to: url, atomically: true, encoding: .utf8)
            pruneOldBackups(for: filePath)
            return FileBackup(id: filename, filePath: filePath, date: Date(), url: url)
        } catch {
            print("[BackupService] Failed to save backup: \(error)")
            return nil
        }
    }
    
    /// Returns all backups for a file, sorted by date (newest first)
    func getBackups(for path: String) -> [FileBackup] {
        let dir = directory(for: path)
        
        guard let files = try? fileManager.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.creationDateKey]) else {
            return []
        }
        
        return files
            .filter { $0.pathExtension == "txt" }
            .compactMap { url -> FileBackup? in
                let filename = url.deletingPathExtension().lastPathComponent
                guard let date = parseDate(from: filename) else { return nil }
                return FileBackup(id: url.lastPathComponent, filePath: path, date: date, url: url)
            }
            .sorted { $0.date > $1.date }
    }
    
    /// Loads the content of a backup
    func loadBackup(_ backup: FileBackup) -> String? {
        try? String(contentsOf: backup.url, encoding: .utf8)
    }
    
    /// Deletes a single backup
    func deleteBackup(_ backup: FileBackup) {
        try? fileManager.removeItem(at: backup.url)
    }
    
    /// Clears all backups for a file
    func clearBackups(for path: String) {
        let dir = directory(for: path)
        if let files = try? fileManager.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) {
            for file in files {
                try? fileManager.removeItem(at: file)
            }
        }
    }
    
    /// Removes oldest backups when exceeding max count
    private func pruneOldBackups(for path: String) {
        let backups = getBackups(for: path)
        if backups.count > maxBackupsPerFile {
            let toDelete = backups.suffix(from: maxBackupsPerFile)
            for backup in toDelete {
                deleteBackup(backup)
            }
        }
    }
    
    /// Parses date from ISO8601 timestamp filename
    private func parseDate(from filename: String) -> Date? {
        let parts = filename.components(separatedBy: "T")
        guard parts.count == 2 else { return nil }
        
        let datePart = parts[0]
        let timePart = parts[1].replacingOccurrences(of: "Z", with: "")
        let timeComponents = timePart.components(separatedBy: "-")
        guard timeComponents.count >= 3 else { return nil }
        
        let reconstructed = "\(datePart)T\(timeComponents[0]):\(timeComponents[1]):\(timeComponents[2])Z"
        return ISO8601DateFormatter().date(from: reconstructed)
    }
}
