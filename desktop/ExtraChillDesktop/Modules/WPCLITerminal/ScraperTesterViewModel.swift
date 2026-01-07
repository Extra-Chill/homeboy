import Foundation
import SwiftUI
import AppKit

@MainActor
class ScraperTesterViewModel: ObservableObject {
    @Published var targetUrl = ""
    @Published var venueName = ""
    @Published var maxResults = 3
    @Published var doUpsert = false
    @Published var output = ""
    @Published var isRunning = false
    
    @AppStorage("localWPPath") private var localWPPath = "/Users/chubes/Developer/LocalWP/testing-grounds/app/public"
    
    private var process: Process?
    
    private let eventsSiteUrl = "testing-grounds.local/events"
    
    func runTest() {
        guard !targetUrl.isEmpty, !isRunning else { return }
        
        guard let environment = LocalEnvironment.buildEnvironment() else {
            output = "Error: Local by Flywheel PHP not detected.\n"
            return
        }
        
        isRunning = true
        output = "Running scraper test...\n"
        output += "Target URL: \(targetUrl)\n"
        if !venueName.isEmpty {
            output += "Venue Override: \(venueName)\n"
        }
        output += "Max Results: \(maxResults)\n"
        output += "Upsert: \(doUpsert ? "Yes" : "No")\n"
        output += "\n"
        
        let process = Process()
        self.process = process
        
        process.currentDirectoryURL = URL(fileURLWithPath: localWPPath)
        process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
        process.environment = environment
        
        var args = ["datamachine-events", "test-scraper"]
        args.append("--target_url=\(targetUrl)")
        args.append("--max=\(maxResults)")
        
        if !venueName.isEmpty {
            args.append("--venue_name=\(venueName)")
        }
        
        if doUpsert {
            args.append("--upsert")
        }
        
        args.append("--url=\(eventsSiteUrl)")
        
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
                self?.output += "\n--- Test Complete ---\n"
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
    
    func cancelTest() {
        process?.terminate()
        isRunning = false
        output += "\n--- Cancelled ---\n"
    }
    
    func clearOutput() {
        output = ""
    }
    
    func copyOutput() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(output, forType: .string)
    }
}
