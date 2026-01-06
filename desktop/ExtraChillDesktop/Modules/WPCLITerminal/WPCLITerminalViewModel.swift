import Foundation
import SwiftUI

@MainActor
class WPCLITerminalViewModel: ObservableObject {
    @Published var command = ""
    @Published var output = ""
    @Published var isRunning = false
    @Published var commandHistory: [String] = []
    @Published var historyIndex = -1
    
    @AppStorage("localWPPath") var localWPPath = "/Users/chubes/Developer/LocalWP/testing-grounds/app/public"
    @AppStorage("savedCommands") private var savedCommandsData = Data()
    
    var savedCommands: [SavedCommand] {
        get {
            (try? JSONDecoder().decode([SavedCommand].self, from: savedCommandsData)) ?? defaultCommands
        }
        set {
            savedCommandsData = (try? JSONEncoder().encode(newValue)) ?? Data()
        }
    }
    
    private var process: Process?
    
    private var defaultCommands: [SavedCommand] {
        [
            SavedCommand(name: "Core Version", command: "wp core version"),
            SavedCommand(name: "Plugin List", command: "wp plugin list --status=active"),
            SavedCommand(name: "Clear Cache", command: "wp cache flush"),
            SavedCommand(name: "DB Export", command: "wp db export"),
        ]
    }
    
    func runCommand() {
        guard !command.isEmpty, !isRunning else { return }
        
        // Add to history
        if commandHistory.last != command {
            commandHistory.append(command)
        }
        historyIndex = commandHistory.count
        
        executeCommand(command)
        command = ""
    }
    
    func runSavedCommand(_ savedCommand: SavedCommand) {
        guard !isRunning else { return }
        executeCommand(savedCommand.command)
    }
    
    private func executeCommand(_ cmd: String) {
        isRunning = true
        output += "$ \(cmd)\n"
        
        let process = Process()
        self.process = process
        
        process.currentDirectoryURL = URL(fileURLWithPath: localWPPath)
        process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
        
        // Parse command string into arguments (simple split, doesn't handle quoted strings perfectly)
        let args = cmd.replacingOccurrences(of: "wp ", with: "")
            .components(separatedBy: " ")
            .filter { !$0.isEmpty }
        
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
    
    func addSavedCommand(name: String, command: String) {
        var commands = savedCommands
        commands.append(SavedCommand(name: name, command: command))
        savedCommands = commands
    }
    
    func removeSavedCommand(_ savedCommand: SavedCommand) {
        var commands = savedCommands
        commands.removeAll { $0.id == savedCommand.id }
        savedCommands = commands
    }
}

struct SavedCommand: Codable, Identifiable, Equatable {
    var id = UUID()
    let name: String
    let command: String
}
