import AppKit
import Combine
import Foundation
import SwiftUI

/// ViewModel for managing a single module's execution state
@MainActor
class ModuleViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()
    let moduleId: String
    
    @Published var inputValues: [String: String] = [:]
    @Published var selectedNetworkSite: String?
    @Published var isRunning = false
    @Published var isSettingUp = false
    @Published var consoleOutput = ""
    @Published var results: [[String: AnyCodableValue]] = []
    @Published var selectedRows: Set<Int> = []
    @Published var error: (any DisplayableError)?
    @Published var actionResult: String?
    @Published var isPerformingAction = false
    

    private let configManager = ConfigurationManager.shared
    
    /// Current module from manager (always up-to-date)
    private var module: LoadedModule? {
        ModuleManager.shared.module(withId: moduleId)
    }
    
    /// Whether this module is a CLI module with subtarget support
    var isCLIModule: Bool {
        module?.manifest.runtime.type == .cli
    }
    
    /// Whether the current project has subtargets (for showing site selector)
    var hasSubTargets: Bool {
        configManager.safeActiveProject.hasSubTargets
    }
    
    /// Available subtargets for CLI modules (e.g., multisite blogs)
    var subTargets: [SubTarget] {
        configManager.safeActiveProject.subTargets
    }
    
    init(moduleId: String) {
        self.moduleId = moduleId
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch:
            // Full reset on project switch
            results = []
            selectedRows = []
            consoleOutput = ""
            error = nil
            actionResult = nil
            selectedNetworkSite = nil
        case .projectModified:
            // Trigger UI refresh for subtarget changes
            objectWillChange.send()
        default:
            break
        }
    }
    
    /// Initialize input values from module manifest
    func initializeInputValues(from module: LoadedModule) {
        // Set default network site for CLI modules
        if module.manifest.runtime.type == .cli {
            selectedNetworkSite = module.manifest.runtime.defaultSite ?? "main"
        }
        
        // Set default input values
        for input in module.manifest.inputs {
            if let defaultValue = input.default {
                inputValues[input.id] = defaultValue.stringValue
            } else {
                inputValues[input.id] = ""
            }
        }
    }
    
    // MARK: - Module Execution
    
    func run(module: LoadedModule) {
        guard !isRunning && !isSettingUp else { return }
        
        consoleOutput = ""
        results = []
        selectedRows = []
        error = nil
        actionResult = nil
        isRunning = true
        
        Task {
            let projectId = configManager.activeProject?.id

            await ModuleManager.shared.runModule(
                moduleId: module.id,
                inputs: inputValues,
                projectId: projectId,
                onOutput: { [weak self] line in
                    Task { @MainActor in
                        self?.consoleOutput += line
                    }
                }
            )

            let output = parseScriptOutput(from: consoleOutput)
            handleRunResult(output, module: module)
        }
    }
    
    private func parseScriptOutput(from output: String) -> Result<ScriptOutput, Error> {
        // Find JSON in output - modules may output non-JSON before the result
        guard let jsonStart = output.lastIndex(of: "{"),
              let jsonEnd = output.lastIndex(of: "}"),
              jsonStart < jsonEnd else {
            return .failure(NSError(domain: "ModuleViewModel", code: 1, userInfo: [NSLocalizedDescriptionKey: "No JSON output found"]))
        }

        let jsonString = String(output[jsonStart...jsonEnd])
        guard let data = jsonString.data(using: .utf8) else {
            return .failure(NSError(domain: "ModuleViewModel", code: 2, userInfo: [NSLocalizedDescriptionKey: "Invalid UTF-8 in output"]))
        }

        do {
            let scriptOutput = try JSONDecoder().decode(ScriptOutput.self, from: data)
            return .success(scriptOutput)
        } catch {
            return .failure(error)
        }
    }

    private func handleRunResult(_ result: Result<ScriptOutput, Error>, module: LoadedModule) {
        isRunning = false
        
        switch result {
        case .success(let output):
            if output.success {
                results = output.results ?? []
                // Auto-select all rows if selectable
                if module.manifest.output.selectable {
                    selectedRows = Set(results.indices)
                }
                if let errors = output.errors, !errors.isEmpty {
                    consoleOutput += "\nWarnings:\n" + errors.joined(separator: "\n")
                }
            } else {
                var message = "Script completed but reported failure"
                if let errors = output.errors, !errors.isEmpty {
                    message += ": " + errors.joined(separator: ", ")
                }
                error = AppError(message, source: "Module: \(moduleId)")
            }
            
        case .failure(let err):
            error = AppError(err.localizedDescription, source: "Module: \(moduleId)")
        }
    }
    
    func cancel() {
        isRunning = false
    }
    
    // MARK: - Module Setup
    
    func setup(module: LoadedModule) {
        guard !isSettingUp && !isRunning else { return }
        
        isSettingUp = true
        consoleOutput = ""
        error = nil
        
        Task {
            do {
                try await ModuleManager.shared.setupModule(moduleId: module.id)
                ModuleManager.shared.updateModuleState(moduleId: module.id, state: .ready)
            } catch {
                self.error = error.toDisplayableError(source: "Module: \(moduleId)")
            }

            isSettingUp = false
        }
    }
    
    // MARK: - Row Selection
    
    func toggleRowSelection(_ index: Int) {
        if selectedRows.contains(index) {
            selectedRows.remove(index)
        } else {
            selectedRows.insert(index)
        }
    }
    
    func selectAll() {
        selectedRows = Set(results.indices)
    }
    
    func deselectAll() {
        selectedRows.removeAll()
    }
    
    var selectedResults: [[String: AnyCodableValue]] {
        selectedRows.sorted().compactMap { index in
            guard index < results.count else { return nil }
            return results[index]
        }
    }
    
    // MARK: - Actions
    
    func performAction(_ action: ActionConfig, module: LoadedModule) async {
        isPerformingAction = true
        actionResult = nil
        
        switch action.type {
        case .builtin:
            performBuiltinAction(action, module: module)
        case .api:
            await performAPIAction(action, module: module)
        }
        
        isPerformingAction = false
    }
    
    private func performBuiltinAction(_ action: ActionConfig, module: LoadedModule) {
        guard let builtin = action.builtin else { return }
        
        switch builtin {
        case .copyColumn:
            guard let column = action.column else { return }
            let values = selectedResults.compactMap { $0[column]?.stringValue }
            let text = values.joined(separator: "\n")
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            actionResult = "Copied \(values.count) values to clipboard"
            
        case .exportCsv:
            exportToCsv(module: module)
            
        case .copyJson:
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            if let data = try? encoder.encode(selectedResults),
               let json = String(data: data, encoding: .utf8) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(json, forType: .string)
                actionResult = "Copied \(selectedResults.count) rows as JSON"
            }
        }
    }
    
    private func exportToCsv(module: LoadedModule) {
        guard !results.isEmpty else { return }
        
        let columns = module.manifest.output.schema.items?.keys.sorted() ?? []
        guard !columns.isEmpty else { return }
        
        var csv = columns.joined(separator: ",") + "\n"
        
        for row in selectedResults {
            let values = columns.map { col in
                let value = row[col]?.stringValue ?? ""
                // Escape quotes and wrap in quotes if contains comma
                let escaped = value.replacingOccurrences(of: "\"", with: "\"\"")
                return escaped.contains(",") || escaped.contains("\n") ? "\"\(escaped)\"" : escaped
            }
            csv += values.joined(separator: ",") + "\n"
        }
        
        // Show save panel
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.commaSeparatedText]
        panel.nameFieldStringValue = "\(module.id)-export.csv"
        
        if panel.runModal() == .OK, let url = panel.url {
            do {
                try csv.write(to: url, atomically: true, encoding: .utf8)
                actionResult = "Exported \(selectedResults.count) rows to \(url.lastPathComponent)"
            } catch {
                self.error = AppError("Failed to save CSV: \(error.localizedDescription)", source: "Module: \(moduleId)")
            }
        }
    }
    
    private func performAPIAction(_ action: ActionConfig, module: LoadedModule) async {
        guard let endpoint = action.endpoint,
              let method = action.method else {
            error = AppError("Invalid API action configuration", source: "Module: \(moduleId)")
            return
        }
        
        // Get current site's API config
        let siteConfig = configManager.safeActiveProject
        guard siteConfig.api.enabled, !siteConfig.api.baseURL.isEmpty else {
            error = AppError("API not configured for current site. Go to Settings to configure.", source: "Module: \(moduleId)")
            return
        }
        
        // Check auth if required
        if action.requiresAuth == true {
            guard await APIClient.shared.hasTokens() else {
                error = AppError("Not logged in. Go to Settings to authenticate.", source: "Module: \(moduleId)")
                return
            }
        }
        
        // Build payload by interpolating templates
        var payload: [String: Any] = [:]
        if let payloadTemplate = action.payload {
            for (key, value) in payloadTemplate {
                payload[key] = interpolateValue(value, module: module)
            }
        }
        
        // Perform request
        do {
            let url = URL(string: siteConfig.api.baseURL + endpoint)!
            var request = URLRequest(url: url)
            request.httpMethod = method
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            
            if action.requiresAuth == true {
                if let token = await APIClient.shared.getAccessToken() {
                    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                }
            }
            
            request.httpBody = try JSONSerialization.data(withJSONObject: payload)
            
            let (data, response) = try await URLSession.shared.data(for: request)
            
            guard let httpResponse = response as? HTTPURLResponse else {
                error = AppError("Invalid response", source: "Module: \(moduleId)")
                return
            }
            
            if httpResponse.statusCode >= 200 && httpResponse.statusCode < 300 {
                // Try to parse response for display
                if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                    actionResult = formatAPIResponse(json)
                } else {
                    actionResult = "Action completed successfully"
                }
            } else {
                let errorText = String(data: data, encoding: .utf8) ?? "Unknown error"
                error = AppError("API error (\(httpResponse.statusCode)): \(errorText)", source: "Module: \(moduleId)")
            }
            
        } catch {
            self.error = AppError("Request failed: \(error.localizedDescription)", source: "Module: \(moduleId)")
        }
    }
    
    /// Interpolates template values like {{selected}} and {{settings.key}}
    private func interpolateValue(_ value: PayloadValue, module: LoadedModule) -> Any {
        switch value {
        case .string(let template):
            if template == "{{selected}}" {
                // Return selected results as array of dicts
                return selectedResults.map { row in
                    var dict: [String: String] = [:]
                    for (key, value) in row {
                        dict[key] = value.stringValue
                    }
                    return dict
                }
            } else if template.hasPrefix("{{settings.") && template.hasSuffix("}}") {
                return ""
            }
            return template
            
        case .array(let arr):
            return arr
        }
    }
    
    private func formatAPIResponse(_ json: [String: Any]) -> String {
        var parts: [String] = []
        
        if let subscribed = json["subscribed"] as? Int {
            parts.append("Subscribed: \(subscribed)")
        }
        if let alreadySubscribed = json["already_subscribed"] as? Int, alreadySubscribed > 0 {
            parts.append("Already subscribed: \(alreadySubscribed)")
        }
        if let failed = json["failed"] as? Int, failed > 0 {
            parts.append("Failed: \(failed)")
        }
        if let message = json["message"] as? String {
            parts.append(message)
        }
        
        return parts.isEmpty ? "Success" : parts.joined(separator: ", ")
    }
    
    // MARK: - Console
    
    func copyConsoleOutput() {
        let moduleName = module?.name ?? moduleId
        ConsoleOutput(consoleOutput, source: "Module: \(moduleName)").copyToClipboard()
    }
    
    func clearConsole() {
        consoleOutput = ""
    }
    
}
