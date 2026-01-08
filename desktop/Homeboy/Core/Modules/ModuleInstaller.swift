import Foundation

/// Handles module dependency installation (venv, pip, Playwright)
class ModuleInstaller: ObservableObject {
    @Published var isInstalling = false
    @Published var progress = ""
    
    private var process: Process?
    private let systemPythonPath = "/opt/homebrew/bin/python3"
    private let fileManager = FileManager.default
    
    /// Shared Playwright browsers location
    private var playwrightBrowsersPath: String {
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("Homeboy/playwright-browsers").path
    }
    
    /// Runs complete setup for a module: venv creation, dependency installation, Playwright browsers
    func setupModule(
        _ module: LoadedModule,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        isInstalling = true
        progress = ""
        
        onOutput("Setting up module: \(module.name)\n")
        onOutput("Creating virtual environment...\n")
        
        createVenv(at: module.venvPath, onOutput: onOutput) { [weak self] result in
            switch result {
            case .success:
                onOutput("\nInstalling dependencies...\n")
                self?.installDependencies(
                    module: module,
                    onOutput: onOutput
                ) { depResult in
                    switch depResult {
                    case .success:
                        // Check if Playwright browsers needed
                        if let browsers = module.manifest.runtime.playwrightBrowsers, !browsers.isEmpty {
                            onOutput("\nInstalling Playwright browsers...\n")
                            self?.installPlaywrightBrowsers(
                                module: module,
                                browsers: browsers,
                                onOutput: onOutput,
                                onComplete: { browserResult in
                                    DispatchQueue.main.async {
                                        self?.isInstalling = false
                                    }
                                    switch browserResult {
                                    case .success:
                                        onOutput("\nSetup complete!\n")
                                        onComplete(.success(()))
                                    case .failure(let error):
                                        onComplete(.failure(error))
                                    }
                                }
                            )
                        } else {
                            DispatchQueue.main.async {
                                self?.isInstalling = false
                            }
                            onOutput("\nSetup complete!\n")
                            onComplete(.success(()))
                        }
                    case .failure(let error):
                        DispatchQueue.main.async {
                            self?.isInstalling = false
                        }
                        onComplete(.failure(error))
                    }
                }
            case .failure(let error):
                DispatchQueue.main.async {
                    self?.isInstalling = false
                }
                onComplete(.failure(error))
            }
        }
    }
    
    /// Creates a Python virtual environment
    private func createVenv(
        at path: String,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        let process = Process()
        self.process = process
        
        process.executableURL = URL(fileURLWithPath: systemPythonPath)
        process.arguments = ["-m", "venv", "--copies", path]
        
        print("[ModuleInstaller] Creating venv at: \(path)")
        print("[ModuleInstaller] Using Python: \(systemPythonPath)")
        print("[ModuleInstaller] Arguments: \(process.arguments ?? [])")
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
        }
        
        process.terminationHandler = { proc in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            // Read any remaining output
            let remaining = pipe.fileHandleForReading.readDataToEndOfFile()
            let remainingOutput = String(data: remaining, encoding: .utf8) ?? ""
            
            DispatchQueue.main.async {
                if !remainingOutput.isEmpty {
                    onOutput(remainingOutput)
                }
                
                print("[ModuleInstaller] venv creation exit code: \(proc.terminationStatus)")
                
                if proc.terminationStatus == 0 {
                    onComplete(.success(()))
                } else {
                    print("[ModuleInstaller] venv creation failed with output: \(remainingOutput)")
                    onComplete(.failure(ModuleInstallerError.venvCreationFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
    
    /// Installs Python dependencies via pip
    private func installDependencies(
        module: LoadedModule,
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        let pipPath = "\(module.venvPath)/bin/pip"
        
        let process = Process()
        self.process = process
        
        process.executableURL = URL(fileURLWithPath: pipPath)
        process.arguments = ["install"] + (module.manifest.runtime.dependencies ?? [])
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
        }
        
        process.terminationHandler = { proc in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            // Read any remaining output
            let remaining = pipe.fileHandleForReading.readDataToEndOfFile()
            if !remaining.isEmpty, let line = String(data: remaining, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
            
            DispatchQueue.main.async {
                if proc.terminationStatus == 0 {
                    onComplete(.success(()))
                } else {
                    onComplete(.failure(ModuleInstallerError.dependencyInstallFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
    
    /// Installs Playwright browsers to shared location
    private func installPlaywrightBrowsers(
        module: LoadedModule,
        browsers: [String],
        onOutput: @escaping (String) -> Void,
        onComplete: @escaping (Result<Void, Error>) -> Void
    ) {
        let process = Process()
        self.process = process
        
        process.executableURL = URL(fileURLWithPath: module.venvPythonPath)
        process.arguments = ["-m", "playwright", "install"] + browsers
        
        // Set browser install path
        var environment = ProcessInfo.processInfo.environment
        environment["PLAYWRIGHT_BROWSERS_PATH"] = playwrightBrowsersPath
        process.environment = environment
        
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if !data.isEmpty, let line = String(data: data, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
        }
        
        process.terminationHandler = { proc in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            // Read any remaining output
            let remaining = pipe.fileHandleForReading.readDataToEndOfFile()
            if !remaining.isEmpty, let line = String(data: remaining, encoding: .utf8) {
                DispatchQueue.main.async {
                    onOutput(line)
                }
            }
            
            DispatchQueue.main.async {
                if proc.terminationStatus == 0 {
                    onComplete(.success(()))
                } else {
                    onComplete(.failure(ModuleInstallerError.playwrightInstallFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
    
    /// Cancels the current installation
    func cancel() {
        process?.terminate()
        isInstalling = false
    }
}

// MARK: - Errors

enum ModuleInstallerError: LocalizedError {
    case venvCreationFailed
    case dependencyInstallFailed
    case playwrightInstallFailed
    
    var errorDescription: String? {
        switch self {
        case .venvCreationFailed:
            return "Failed to create Python virtual environment"
        case .dependencyInstallFailed:
            return "Failed to install Python dependencies"
        case .playwrightInstallFailed:
            return "Failed to install Playwright browsers"
        }
    }
}
