import Foundation

/// Represents a local backup of a config file
struct ConfigBackup: Identifiable {
    let id: String
    let file: ConfigFile
    let date: Date
    let url: URL
    
    var displayDate: String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

/// Manages local backups of server configuration files.
/// Backups are stored in ~/Library/Application Support/Homeboy/backups/
class BackupService {
    static let shared = BackupService()
    
    private let fileManager = FileManager.default
    private let maxBackupsPerFile = 10
    
    /// Base directory for backups
    private var backupsDirectory: URL {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy").appendingPathComponent("backups")
    }
    
    private init() {
        ensureDirectoriesExist()
    }
    
    private func ensureDirectoriesExist() {
        for file in ConfigFile.allCases {
            let dir = backupsDirectory.appendingPathComponent(file.rawValue)
            try? fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        }
    }
    
    /// Directory for a specific file's backups
    private func directory(for file: ConfigFile) -> URL {
        backupsDirectory.appendingPathComponent(file.rawValue)
    }
    
    /// Saves a backup of the current server content before overwriting
    /// - Parameters:
    ///   - file: The config file type
    ///   - content: The current server content to back up
    /// - Returns: The created backup, or nil if save failed
    @discardableResult
    func saveBackup(file: ConfigFile, content: String) -> ConfigBackup? {
        let timestamp = ISO8601DateFormatter().string(from: Date())
            .replacingOccurrences(of: ":", with: "-") // Filesystem-safe
        let filename = "\(timestamp).txt"
        let url = directory(for: file).appendingPathComponent(filename)
        
        do {
            try content.write(to: url, atomically: true, encoding: .utf8)
            pruneOldBackups(for: file)
            return ConfigBackup(id: filename, file: file, date: Date(), url: url)
        } catch {
            print("[BackupService] Failed to save backup: \(error)")
            return nil
        }
    }
    
    /// Returns all backups for a file, sorted by date (newest first)
    func getBackups(for file: ConfigFile) -> [ConfigBackup] {
        let dir = directory(for: file)
        
        guard let files = try? fileManager.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.creationDateKey]) else {
            return []
        }
        
        return files
            .filter { $0.pathExtension == "txt" }
            .compactMap { url -> ConfigBackup? in
                let filename = url.deletingPathExtension().lastPathComponent
                guard let date = parseDate(from: filename) else { return nil }
                return ConfigBackup(id: url.lastPathComponent, file: file, date: date, url: url)
            }
            .sorted { $0.date > $1.date }
    }
    
    /// Loads the content of a backup
    func loadBackup(_ backup: ConfigBackup) -> String? {
        try? String(contentsOf: backup.url, encoding: .utf8)
    }
    
    /// Deletes a single backup
    func deleteBackup(_ backup: ConfigBackup) {
        try? fileManager.removeItem(at: backup.url)
    }
    
    /// Clears all backups for a file
    func clearBackups(for file: ConfigFile) {
        let dir = directory(for: file)
        if let files = try? fileManager.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) {
            for file in files {
                try? fileManager.removeItem(at: file)
            }
        }
    }
    
    /// Removes oldest backups when exceeding max count
    private func pruneOldBackups(for file: ConfigFile) {
        let backups = getBackups(for: file)
        if backups.count > maxBackupsPerFile {
            let toDelete = backups.suffix(from: maxBackupsPerFile)
            for backup in toDelete {
                deleteBackup(backup)
            }
        }
    }
    
    /// Parses date from ISO8601 timestamp filename
    private func parseDate(from filename: String) -> Date? {
        // Convert filesystem-safe format back to ISO8601
        let iso8601 = filename.replacingOccurrences(of: "-", with: ":")
            .replacingOccurrences(of: "T:", with: "T") // Fix the T separator
        
        // Try standard ISO8601 parsing
        let formatter = ISO8601DateFormatter()
        if let date = formatter.date(from: iso8601) {
            return date
        }
        
        // Fallback: try to parse the original format
        // Format: 2026-01-07T14-34-22Z
        let parts = filename.components(separatedBy: "T")
        guard parts.count == 2 else { return nil }
        
        let datePart = parts[0] // 2026-01-07
        let timePart = parts[1].replacingOccurrences(of: "Z", with: "") // 14-34-22
        let timeComponents = timePart.components(separatedBy: "-")
        guard timeComponents.count >= 3 else { return nil }
        
        let reconstructed = "\(datePart)T\(timeComponents[0]):\(timeComponents[1]):\(timeComponents[2])Z"
        return formatter.date(from: reconstructed)
    }
}
