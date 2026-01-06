import Foundation

class PythonRunner: ObservableObject {
    @Published var isRunning = false
    @Published var output = ""
    @Published var error: String?
    
    private var process: Process?
    private let pythonPath = "/opt/homebrew/bin/python3"
    
    var venvPath: String {
        let bundlePath = Bundle.main.bundlePath
        return "\(bundlePath)/../venv"
    }
    
    var venvPythonPath: String {
        "\(venvPath)/bin/python3"
    }
    
    var scriptsPath: String {
        Bundle.main.resourcePath ?? ""
    }
    
    func run(script: String, arguments: [String] = [], onOutput: @escaping (String) -> Void, onComplete: @escaping (Result<String, Error>) -> Void) {
        guard !isRunning else {
            onComplete(.failure(PythonError.alreadyRunning))
            return
        }
        
        isRunning = true
        output = ""
        error = nil
        
        let process = Process()
        self.process = process
        
        // Use venv Python if available, otherwise system Python
        let pythonExecutable = FileManager.default.fileExists(atPath: venvPythonPath) ? venvPythonPath : pythonPath
        
        process.executableURL = URL(fileURLWithPath: pythonExecutable)
        
        let scriptPath = "\(scriptsPath)/\(script)"
        process.arguments = [scriptPath] + arguments
        
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe
        
        // Stream stderr for real-time progress updates
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
            // Stop reading stderr
            stderrPipe.fileHandleForReading.readabilityHandler = nil
            
            // Read final stdout (JSON result)
            let stdoutData = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
            let stdout = String(data: stdoutData, encoding: .utf8) ?? ""
            
            DispatchQueue.main.async {
                self?.isRunning = false
                self?.process = nil
                
                if proc.terminationStatus == 0 {
                    onComplete(.success(stdout))
                } else {
                    let errorMessage = self?.output ?? "Unknown error"
                    onComplete(.failure(PythonError.executionFailed(errorMessage)))
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
    
    func cancel() {
        process?.terminate()
        isRunning = false
    }
    
    func checkVenvExists() -> Bool {
        FileManager.default.fileExists(atPath: venvPythonPath)
    }
    
    func setupVenv(onOutput: @escaping (String) -> Void, onComplete: @escaping (Result<Void, Error>) -> Void) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: pythonPath)
        process.arguments = ["-m", "venv", venvPath]
        
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
            
            DispatchQueue.main.async {
                if proc.terminationStatus == 0 {
                    onComplete(.success(()))
                } else {
                    onComplete(.failure(PythonError.venvSetupFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
    
    func installDependencies(onOutput: @escaping (String) -> Void, onComplete: @escaping (Result<Void, Error>) -> Void) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "\(venvPath)/bin/pip")
        process.arguments = ["install", "playwright", "beautifulsoup4", "lxml", "requests", "tldextract"]
        
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
        
        process.terminationHandler = { [weak self] proc in
            pipe.fileHandleForReading.readabilityHandler = nil
            
            if proc.terminationStatus == 0 {
                // Install Playwright browsers
                self?.installPlaywrightBrowsers(onOutput: onOutput, onComplete: onComplete)
            } else {
                DispatchQueue.main.async {
                    onComplete(.failure(PythonError.dependencyInstallFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
    
    private func installPlaywrightBrowsers(onOutput: @escaping (String) -> Void, onComplete: @escaping (Result<Void, Error>) -> Void) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: venvPythonPath)
        process.arguments = ["-m", "playwright", "install", "chromium"]
        
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
            
            DispatchQueue.main.async {
                if proc.terminationStatus == 0 {
                    onComplete(.success(()))
                } else {
                    onComplete(.failure(PythonError.playwrightInstallFailed))
                }
            }
        }
        
        do {
            try process.run()
        } catch {
            onComplete(.failure(error))
        }
    }
}

enum PythonError: LocalizedError {
    case alreadyRunning
    case executionFailed(String)
    case venvSetupFailed
    case dependencyInstallFailed
    case playwrightInstallFailed
    
    var errorDescription: String? {
        switch self {
        case .alreadyRunning:
            return "A script is already running"
        case .executionFailed(let message):
            return "Script execution failed: \(message)"
        case .venvSetupFailed:
            return "Failed to create Python virtual environment"
        case .dependencyInstallFailed:
            return "Failed to install Python dependencies"
        case .playwrightInstallFailed:
            return "Failed to install Playwright browsers"
        }
    }
}
