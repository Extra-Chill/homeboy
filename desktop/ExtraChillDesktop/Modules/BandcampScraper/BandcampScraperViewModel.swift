import Foundation
import SwiftUI

@MainActor
class BandcampScraperViewModel: ObservableObject {
    @Published var tag: String = ""
    @Published var clicks: Int = 3
    @Published var isRunning = false
    @Published var consoleOutput = ""
    @Published var results: [ScrapedEmail] = []
    @Published var selectedEmails: Set<String> = []
    @Published var error: String?
    @Published var isSubscribing = false
    @Published var subscribeResult: String?
    
    @AppStorage("sendyListId") var sendyListId = ""
    @AppStorage("lastUsedTag") var lastUsedTag = ""
    
    private let pythonRunner = PythonRunner()
    
    init() {
        // Load last used tag
        if !lastUsedTag.isEmpty {
            tag = lastUsedTag
        }
    }
    
    func startScrape() {
        guard !isRunning else { return }
        
        // Save tag for next time
        lastUsedTag = tag
        
        isRunning = true
        consoleOutput = ""
        results = []
        selectedEmails = []
        error = nil
        
        let arguments = [
            "--tag", tag,
            "--clicks", String(clicks),
            "--output", "json",
            "--headless", "true"
        ]
        
        pythonRunner.run(
            script: "bandcamp_scraper.py",
            arguments: arguments,
            onOutput: { [weak self] line in
                self?.consoleOutput += line
            },
            onComplete: { [weak self] result in
                self?.handleScraperResult(result)
            }
        )
    }
    
    func cancelScrape() {
        pythonRunner.cancel()
        isRunning = false
    }
    
    private func handleScraperResult(_ result: Result<String, Error>) {
        isRunning = false
        
        switch result {
        case .success(let json):
            do {
                let decoder = JSONDecoder()
                let scraperResult = try decoder.decode(ScraperResult.self, from: Data(json.utf8))
                
                if scraperResult.success {
                    results = scraperResult.results
                    // Auto-select all by default
                    selectedEmails = Set(results.map { $0.email })
                    
                    if !scraperResult.errors.isEmpty {
                        consoleOutput += "\nWarnings:\n" + scraperResult.errors.joined(separator: "\n")
                    }
                } else {
                    error = "Scraper completed but reported failure"
                    if !scraperResult.errors.isEmpty {
                        error! += ": " + scraperResult.errors.joined(separator: ", ")
                    }
                }
            } catch {
                self.error = "Failed to parse results: \(error.localizedDescription)"
                consoleOutput += "\nRaw output:\n\(json)"
            }
            
        case .failure(let err):
            error = err.localizedDescription
        }
    }
    
    func toggleSelection(_ email: String) {
        if selectedEmails.contains(email) {
            selectedEmails.remove(email)
        } else {
            selectedEmails.insert(email)
        }
    }
    
    func selectAll() {
        selectedEmails = Set(results.map { $0.email })
    }
    
    func deselectAll() {
        selectedEmails.removeAll()
    }
    
    func subscribeToNewsletter() async {
        guard !sendyListId.isEmpty else {
            subscribeResult = "Please enter a Sendy List ID in Settings"
            return
        }
        
        guard !selectedEmails.isEmpty else {
            subscribeResult = "No emails selected"
            return
        }
        
        isSubscribing = true
        subscribeResult = nil
        
        let emailsToSubscribe = results
            .filter { selectedEmails.contains($0.email) }
            .map { EmailEntry(email: $0.email, name: $0.name) }
        
        do {
            let response = try await APIClient.shared.bulkSubscribe(
                emails: emailsToSubscribe,
                listId: sendyListId
            )
            
            subscribeResult = "Subscribed: \(response.subscribed), Already subscribed: \(response.alreadySubscribed), Failed: \(response.failed)"
            
            if !response.errors.isEmpty {
                subscribeResult! += "\nErrors: " + response.errors.joined(separator: ", ")
            }
        } catch {
            subscribeResult = "Subscription failed: \(error.localizedDescription)"
        }
        
        isSubscribing = false
    }
    
    func checkPythonSetup() -> Bool {
        pythonRunner.checkVenvExists()
    }
    
    func setupPython(onOutput: @escaping (String) -> Void, onComplete: @escaping (Result<Void, Error>) -> Void) {
        pythonRunner.setupVenv(onOutput: onOutput) { [weak self] result in
            switch result {
            case .success:
                self?.pythonRunner.installDependencies(onOutput: onOutput, onComplete: onComplete)
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
}
