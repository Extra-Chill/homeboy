import Foundation

/// Executes module scripts and handles output streaming
@MainActor
class ModuleRunner: ObservableObject {
    @Published var isRunning = false
    @Published var output = ""
    @Published var error: String?
    
    private var process: Process?
    private let systemPythonPath = "/opt/homebrew/bin/python3"
    private let wpCliPath = "/opt/homebrew/bin/wp"
    
    /// Shared Playwright browsers location
    private var playwrightBrowsersPath: String {
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy/playwright-browsers").path
    }
    
    /// Runs a module's script with the given input values
    func run(
        module: LoadedModule,
        inputValues: [String: String],
        selectedNetworkSite: String? = nil,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<ScriptOutput, Error>) -> Void
    ) {
        guard !isRunning else {
            onComplete(.failure(ModuleRunnerError.alreadyRunning))
            return
        }
        
        guard module.state == .ready else {
            onComplete(.failure(ModuleRunnerError.moduleNotReady))
            return
        }
        
        isRunning = true
        output = ""
        error = nil
        
        switch module.manifest.runtime.type {
        case .python:
            runPythonModule(module: module, inputValues: inputValues, onOutput: onOutput, onComplete: onComplete)
        case .shell:
            runShellModule(module: module, inputValues: inputValues, onOutput: onOutput, onComplete: onComplete)
        case .wpcli:
            runWPCLIModule(module: module, inputValues: inputValues, selectedNetworkSite: selectedNetworkSite, onOutput: onOutput, onComplete: onComplete)
        }
    }
    
    // MARK: - Python Modules
    
    private func runPythonModule(
        module: LoadedModule,
        inputValues: [String: String],
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<ScriptOutput, Error>) -> Void
    ) {
        let process = Process()
        self.process = process
        
        // Determine Python executable
        let pythonPath = FileManager.default.fileExists(atPath: module.venvPythonPath)
            ? module.venvPythonPath
            : systemPythonPath
        
        process.executableURL = URL(fileURLWithPath: pythonPath)
        
        // Build arguments from manifest inputs
        var arguments = [module.entrypointPath]
        for input in module.manifest.inputs {
            if let value = inputValues[input.id], !value.isEmpty {
                arguments.append(input.arg)
                arguments.append(value)
            }
        }
        
        process.arguments = arguments
        
        // Set environment for Playwright
        var environment = ProcessInfo.processInfo.environment
        environment["PLAYWRIGHT_BROWSERS_PATH"] = playwrightBrowsersPath
        process.environment = environment
        
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe
        
        // Stream stderr for real-time progress
        stderrPipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                    onOutput(line)
                }
            }
        }
        
        process.terminationHandler = { [weak self] proc in
            stderrPipe.fileHandleForReading.readabilityHandler = nil
            
            let remainingStderr = stderrPipe.fileHandleForReading.readDataToEndOfFile()
            if !remainingStderr.isEmpty, let line = String(data: remainingStderr, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                    onOutput(line)
                }
            }
            
            let stdoutData = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
            let stdout = String(data: stdoutData, encoding: .utf8) ?? ""
            
            DispatchQueue.main.async {
                self?.isRunning = false
                self?.process = nil
                
                if proc.terminationStatus == 0 {
                    do {
                        let result = try JSONDecoder().decode(ScriptOutput.self, from: Data(stdout.utf8))
                        onComplete(.success(result))
                    } catch {
                        onComplete(.failure(ModuleRunnerError.invalidOutput(stdout)))
                    }
                } else {
                    let errorMessage = self?.output ?? "Unknown error"
                    onComplete(.failure(ModuleRunnerError.executionFailed(errorMessage)))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            isRunning = false
            self.process = nil
            onComplete(.failure(error))
        }
    }
    
    // MARK: - Shell Modules
    
    private func runShellModule(
        module: LoadedModule,
        inputValues: [String: String],
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<ScriptOutput, Error>) -> Void
    ) {
        let process = Process()
        self.process = process
        
        process.executableURL = URL(fileURLWithPath: "/bin/bash")
        
        var arguments = [module.entrypointPath]
        for input in module.manifest.inputs {
            if let value = inputValues[input.id], !value.isEmpty {
                arguments.append(input.arg)
                arguments.append(value)
            }
        }
        
        process.arguments = arguments
        
        runProcessWithConsoleOutput(process: process, onOutput: onOutput, onComplete: onComplete)
    }
    
    // MARK: - WP-CLI Modules
    
    private func runWPCLIModule(
        module: LoadedModule,
        inputValues: [String: String],
        selectedNetworkSite: String?,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<ScriptOutput, Error>) -> Void
    ) {
        guard let environment = LocalEnvironment.buildEnvironment() else {
            isRunning = false
            onOutput("Error: Local by Flywheel PHP not detected.\n")
            onComplete(.failure(ModuleRunnerError.executionFailed("Local by Flywheel PHP not detected")))
            return
        }
        
        let config = ConfigurationManager.shared.safeActiveProject
        
        let process = Process()
        self.process = process
        
        process.currentDirectoryURL = URL(fileURLWithPath: config.localDev.wpCliPath)
        process.executableURL = URL(fileURLWithPath: wpCliPath)
        process.environment = environment
        
        // Build WP-CLI command
        var arguments: [String] = []
        
        // Add command and subcommand
        if let command = module.manifest.runtime.command {
            arguments.append(command)
        }
        if let subcommand = module.manifest.runtime.subcommand {
            arguments.append(subcommand)
        }
        
        // Add input arguments
        for input in module.manifest.inputs {
            if let value = inputValues[input.id], !value.isEmpty {
                arguments.append("\(input.arg)=\(value)")
            }
        }
        
        // Add --url flag for multisite
        if let multisite = config.multisite, multisite.enabled {
            let siteId = selectedNetworkSite ?? module.manifest.runtime.defaultSite ?? "main"
            let localDomain = config.localDev.domain.isEmpty ? "localhost" : config.localDev.domain
            
            if let blog = multisite.blogs.first(where: { $0.name.lowercased() == siteId.lowercased() }) {
                let urlPath = blog.blogId == 1 ? "" : "/\(blog.name.lowercased())"
                arguments.append("--url=\(localDomain)\(urlPath)")
            } else {
                // Default to main site
                arguments.append("--url=\(localDomain)")
            }
        }
        
        process.arguments = arguments
        
        // Log the command being run
        let commandString = "wp \(arguments.joined(separator: " "))"
        onOutput("$ \(commandString)\n\n")
        
        runProcessWithConsoleOutput(process: process, onOutput: onOutput, onComplete: onComplete)
    }
    
    // MARK: - Shared Process Execution
    
    private func runProcessWithConsoleOutput(
        process: Process,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<ScriptOutput, Error>) -> Void
    ) {
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                    onOutput(line)
                }
            }
        }
        
        process.terminationHandler = { [weak self] proc in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            let remainingData = pipe.fileHandleForReading.readDataToEndOfFile()
            if !remainingData.isEmpty, let line = String(data: remainingData, encoding: .utf8) {
                DispatchQueue.main.async {
                    self?.output += line
                    onOutput(line)
                }
            }
            
            DispatchQueue.main.async {
                self?.isRunning = false
                self?.process = nil
                
                // For console-output modules (shell, wpcli), return success with empty results
                let success = proc.terminationStatus == 0
                let result = ScriptOutput(success: success, results: nil, errors: success ? nil : [self?.output ?? ""])
                onComplete(.success(result))
            }
        }
        
        do {
            try process.run()
        } catch {
            isRunning = false
            self.process = nil
            onComplete(.failure(error))
        }
    }
    
    /// Cancels the currently running process
    func cancel() {
        process?.terminate()
        isRunning = false
    }
}

// MARK: - Errors

enum ModuleRunnerError: LocalizedError {
    case alreadyRunning
    case moduleNotReady
    case executionFailed(String)
    case invalidOutput(String)
    
    var errorDescription: String? {
        switch self {
        case .alreadyRunning:
            return "A script is already running"
        case .moduleNotReady:
            return "Module is not ready (needs setup)"
        case .executionFailed(let message):
            return "Script execution failed: \(message)"
        case .invalidOutput(let output):
            return "Invalid script output (expected JSON): \(output.prefix(200))"
        }
    }
}
