import Foundation
import SwiftUI

struct NetworkSite: Identifiable, Hashable {
    let id: String
    let name: String
    let path: String
    let blogId: Int
    
    var urlFlag: String {
        "testing-grounds.local\(path)"
    }
}

@MainActor
class WPCLITerminalViewModel: ObservableObject {
    @Published var command = ""
    @Published var output = ""
    @Published var isRunning = false
    @Published var commandHistory: [String] = []
    @Published var historyIndex = -1
    
    @AppStorage("localWPPath") var localWPPath = "/Users/chubes/Developer/LocalWP/testing-grounds/app/public"
    @AppStorage("selectedSiteId") private var selectedSiteId = "main"
    
    private var process: Process?
    
    static let networkSites: [NetworkSite] = [
        NetworkSite(id: "main", name: "Main", path: "", blogId: 1),
        NetworkSite(id: "community", name: "Community", path: "/community", blogId: 2),
        NetworkSite(id: "shop", name: "Shop", path: "/shop", blogId: 3),
        NetworkSite(id: "artist", name: "Artist", path: "/artist", blogId: 4),
        NetworkSite(id: "chat", name: "Chat", path: "/chat", blogId: 5),
        NetworkSite(id: "events", name: "Events", path: "/events", blogId: 7),
        NetworkSite(id: "stream", name: "Stream", path: "/stream", blogId: 8),
        NetworkSite(id: "newsletter", name: "Newsletter", path: "/newsletter", blogId: 9),
        NetworkSite(id: "docs", name: "Docs", path: "/docs", blogId: 10),
        NetworkSite(id: "wire", name: "Wire", path: "/wire", blogId: 11),
        NetworkSite(id: "horoscope", name: "Horoscope", path: "/horoscope", blogId: 12),
    ]
    
    var selectedSite: NetworkSite {
        Self.networkSites.first { $0.id == selectedSiteId } ?? Self.networkSites[0]
    }
    
    func selectSite(_ site: NetworkSite) {
        selectedSiteId = site.id
    }
    
    func runCommand() {
        guard !command.isEmpty, !isRunning else { return }
        
        if commandHistory.last != command {
            commandHistory.append(command)
        }
        historyIndex = commandHistory.count
        
        executeCommand(command)
        command = ""
    }
    
    private func executeCommand(_ cmd: String) {
        isRunning = true
        output += "$ \(cmd) --url=\(selectedSite.urlFlag)\n"
        
        guard let environment = LocalEnvironment.buildEnvironment() else {
            output += "Error: Local by Flywheel PHP not detected.\n"
            output += "Please ensure Local is installed and has a PHP version available.\n"
            output += "Expected path: ~/Library/Application Support/Local/lightning-services/php-*/\n"
            isRunning = false
            return
        }
        
        let process = Process()
        self.process = process
        
        process.currentDirectoryURL = URL(fileURLWithPath: localWPPath)
        process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
        process.environment = environment
        
        var args = cmd.replacingOccurrences(of: "wp ", with: "")
            .components(separatedBy: " ")
            .filter { !$0.isEmpty }
        
        // Append --url flag for multisite targeting
        args.append("--url=\(selectedSite.urlFlag)")
        
        process.arguments = args
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                }
            }
        }
        
        process.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async {
                pipe.fileHandleForReading.readabilityHandler = nil
                self?.output += "\n"
                self?.isRunning = false
                self?.process = nil
            }
        }
        
        do {
            try process.run()
        } catch {
            output += "Error: \(error.localizedDescription)\n"
            isRunning = false
        }
    }
    
    func cancelCommand() {
        process?.terminate()
        isRunning = false
    }
    
    func clearOutput() {
        output = ""
    }
    
    func navigateHistory(direction: Int) {
        let newIndex = historyIndex + direction
        
        if newIndex >= 0 && newIndex < commandHistory.count {
            historyIndex = newIndex
            command = commandHistory[newIndex]
        } else if newIndex >= commandHistory.count {
            historyIndex = commandHistory.count
            command = ""
        }
    }
}
