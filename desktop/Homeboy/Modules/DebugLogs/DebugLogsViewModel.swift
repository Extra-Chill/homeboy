import AppKit
import Combine
import Foundation
import SwiftUI

enum LineCount: Int, CaseIterable, Identifiable {
    case hundred = 100
    case fiveHundred = 500
    case thousand = 1000
    case all = 0
    
    var id: Int { rawValue }
    
    var displayName: String {
        switch self {
        case .hundred: return "100"
        case .fiveHundred: return "500"
        case .thousand: return "1000"
        case .all: return "All"
        }
    }
}

@MainActor
class DebugLogsViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    
    @Published var logContent = ""
    @Published var isLoading = false
    @Published var error: String?
    @Published var lastUpdated: Date?
    @Published var fileSize: Int64 = 0
    @Published var searchText = ""
    @Published var selectedLineCount: LineCount = .fiveHundred
    
    private var sshService: SSHService?
    private var wpContentPath: String?
    
    var filteredContent: String {
        guard !searchText.isEmpty else { return logContent }
        
        let lines = logContent.components(separatedBy: "\n")
        let filtered = lines.filter { $0.localizedCaseInsensitiveContains(searchText) }
        return filtered.joined(separator: "\n")
    }
    
    var fileSizeFormatted: String {
        if fileSize == 0 { return "0 B" }
        
        let units = ["B", "KB", "MB", "GB"]
        var size = Double(fileSize)
        var unitIndex = 0
        
        while size >= 1024 && unitIndex < units.count - 1 {
            size /= 1024
            unitIndex += 1
        }
        
        if unitIndex == 0 {
            return "\(Int(size)) \(units[unitIndex])"
        }
        return String(format: "%.1f %@", size, units[unitIndex])
    }
    
    var lastUpdatedFormatted: String {
        guard let date = lastUpdated else { return "Never" }
        
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: date, relativeTo: Date())
    }
    
    init() {
        setupSSH()
    }
    
    private func setupSSH() {
        let project = ConfigurationManager.readCurrentProject()
        sshService = SSHService()
        wpContentPath = project.wordpress?.wpContentPath
    }
    
    func fetchLogs() async {
        guard let ssh = sshService, let wpPath = wpContentPath, !wpPath.isEmpty else {
            error = "SSH or WordPress not configured. Check Settings."
            return
        }
        
        isLoading = true
        error = nil
        
        let command: String
        if selectedLineCount == .all {
            command = "cat \(wpPath)/debug.log 2>/dev/null || echo ''"
        } else {
            command = "tail -n \(selectedLineCount.rawValue) \(wpPath)/debug.log 2>/dev/null || echo ''"
        }
        
        do {
            let output = try await ssh.executeCommandSync(command)
            logContent = output.trimmingCharacters(in: .whitespacesAndNewlines)
            lastUpdated = Date()
            
            await fetchFileSize()
        } catch {
            self.error = error.localizedDescription
        }
        
        isLoading = false
    }
    
    func fetchFileSize() async {
        guard let ssh = sshService, let wpPath = wpContentPath, !wpPath.isEmpty else { return }
        
        // Linux stat uses -c format, macOS uses -f format
        let command = "stat -c '%s' \(wpPath)/debug.log 2>/dev/null || stat -f '%z' \(wpPath)/debug.log 2>/dev/null || echo '0'"
        
        do {
            let output = try await ssh.executeCommandSync(command)
            let sizeString = output.trimmingCharacters(in: .whitespacesAndNewlines)
            fileSize = Int64(sizeString) ?? 0
        } catch {
            fileSize = 0
        }
    }
    
    func clearLogs() async {
        guard let ssh = sshService, let wpPath = wpContentPath, !wpPath.isEmpty else {
            error = "SSH or WordPress not configured. Check Settings."
            return
        }
        
        isLoading = true
        error = nil
        
        let command = "rm -f \(wpPath)/debug.log"
        
        do {
            _ = try await ssh.executeCommandSync(command)
            logContent = ""
            fileSize = 0
            lastUpdated = Date()
        } catch {
            self.error = error.localizedDescription
        }
        
        isLoading = false
    }
    
    func copyLogs() {
        let contentToCopy = searchText.isEmpty ? logContent : filteredContent
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(contentToCopy, forType: .string)
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
        // Clear content
        logContent = ""
        fileSize = 0
        lastUpdated = nil
        error = nil
        searchText = ""
        
        // Recreate SSH service with new site config
        setupSSH()
    }
}
