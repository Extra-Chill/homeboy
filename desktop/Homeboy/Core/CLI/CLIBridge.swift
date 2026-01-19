import Foundation


/// Response structure for CLI execution results
struct CLIBridgeResponse: Sendable {
    let success: Bool
    let output: String
    let errorOutput: String
    let exitCode: Int32

    /// Shared decoder configured for snake_case keys
    private static var decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    /// Attempts to decode the output as a JSON response with the given data type
    func decode<T: Decodable>(_ type: T.Type) throws -> T {
        guard success else {
            throw CLIBridgeError.executionFailed(exitCode: exitCode, message: errorOutput)
        }

        guard let data = output.data(using: .utf8) else {
            throw CLIBridgeError.invalidResponse("Output is not valid UTF-8")
        }

        return try Self.decoder.decode(type, from: data)
    }

    /// Attempts to decode as a standard CLIResponse structure
    func decodeResponse<T: Decodable>(_ dataType: T.Type) throws -> CLIBridgeResult<T> {
        guard let data = output.data(using: .utf8) else {
            throw CLIBridgeError.invalidResponse("Output is not valid UTF-8")
        }

        return try Self.decoder.decode(CLIBridgeResult<T>.self, from: data)
    }
}

/// Standard response structure matching CLI output contract
struct CLIBridgeResult<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
    let error: CLIBridgeErrorDetail?
}

/// Error detail from CLI response matching the full CLI JSON contract
struct CLIBridgeErrorDetail: Decodable, Sendable {
    let code: String
    let message: String
    let details: [String: JSONValue]?
    let hints: [CLIHint]?
    let retryable: Bool?

    /// Convert to a CLIError with source context for UI display
    func toCLIError(source: String) -> CLIError {
        CLIError(
            code: code,
            message: message,
            details: details ?? [:],
            hints: hints ?? [],
            retryable: retryable,
            source: source
        )
    }
}

/// Errors that can occur during CLI bridge operations
enum CLIBridgeError: LocalizedError {
    case notInstalled
    case executionFailed(exitCode: Int32, message: String)
    case invalidResponse(String)
    case timeout
    case cliError(CLIError)

    var errorDescription: String? {
        switch self {
        case .notInstalled:
            return "Homeboy CLI is not installed. Install via Settings → CLI."
        case .executionFailed(let code, let message):
            return "CLI command failed (exit \(code)): \(message)"
        case .invalidResponse(let reason):
            return "Invalid CLI response: \(reason)"
        case .timeout:
            return "CLI command timed out"
        case .cliError(let error):
            return error.message
        }
    }

    /// Extract the underlying CLIError if this is a structured CLI error
    var cliError: CLIError? {
        if case .cliError(let error) = self {
            return error
        }
        return nil
    }
}

/// Bridge for executing CLI commands from the GUI.
/// This allows the GUI to shell out to the CLI binary instead of importing Core/ services directly.
actor CLIBridge {
    static let shared = CLIBridge()

    private init() {}

    private actor ProcessOutputCollector {
        private var stdoutData = Data()
        private var stderrData = Data()

        func appendStdout(_ data: Data) {
            stdoutData.append(data)
        }

        func appendStderr(_ data: Data) {
            stderrData.append(data)
        }

        func finalize() -> (stdout: String, stderr: String) {
            (
                String(data: stdoutData, encoding: .utf8) ?? "",
                String(data: stderrData, encoding: .utf8) ?? ""
            )
        }
    }

    private actor ContinuationGate {
        private var didResume = false

        func resumeOnce<T>(
            _ continuation: CheckedContinuation<T, Error>,
            with result: Result<T, Error>
        ) {
            guard !didResume else { return }
            didResume = true

            continuation.resume(with: result)
        }
    }

    // MARK: - Installation Status

    /// Whether the CLI is installed (delegates to CLIVersionChecker)
    nonisolated var isInstalled: Bool {
        CLIVersionChecker.shared.isInstalled
    }

    /// Get CLI path from version checker (single source of truth)
    private func cliPath() async -> String? {
        await CLIVersionChecker.shared.cliPath()
    }

    // MARK: - Command Execution

    /// Executes a CLI command and returns the result
    /// - Parameters:
    ///   - args: Arguments to pass to the CLI (e.g., ["project", "show", "mysite"])
    ///   - timeout: Maximum time to wait for the command to complete
    /// - Returns: The command result including output and exit code
    func execute(_ args: [String], timeout: TimeInterval = 30) async throws -> CLIBridgeResponse {
        guard let path = await cliPath() else {
            throw CLIBridgeError.notInstalled
        }

        return try await withCheckedThrowingContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: path)
            process.arguments = args

            let stdoutPipe = Pipe()
            let stderrPipe = Pipe()
            process.standardOutput = stdoutPipe
            process.standardError = stderrPipe

            let outputCollector = ProcessOutputCollector()
            let continuationGate = ContinuationGate()

            stdoutPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty else { return }

                Task {
                    await outputCollector.appendStdout(data)
                }
            }

            stderrPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty else { return }

                Task {
                    await outputCollector.appendStderr(data)
                }
            }

            let timeoutWorkItem = DispatchWorkItem {
                Task {
                    if process.isRunning {
                        process.terminate()
                    }

                    await continuationGate.resumeOnce(continuation, with: .failure(CLIBridgeError.timeout))
                }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timeoutWorkItem)

            process.terminationHandler = { proc in
                timeoutWorkItem.cancel()

                stdoutPipe.fileHandleForReading.readabilityHandler = nil
                stderrPipe.fileHandleForReading.readabilityHandler = nil

                Task {
                    let remainingStdout = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
                    let remainingStderr = stderrPipe.fileHandleForReading.readDataToEndOfFile()

                    if !remainingStdout.isEmpty {
                        await outputCollector.appendStdout(remainingStdout)
                    }
                    if !remainingStderr.isEmpty {
                        await outputCollector.appendStderr(remainingStderr)
                    }

                    let output = await outputCollector.finalize()
                    let response = CLIBridgeResponse(
                        success: proc.terminationStatus == 0,
                        output: output.stdout,
                        errorOutput: output.stderr,
                        exitCode: proc.terminationStatus
                    )

                    await continuationGate.resumeOnce(continuation, with: .success(response))
                }
            }

            do {
                try process.run()
            } catch {
                timeoutWorkItem.cancel()
                Task {
                    await continuationGate.resumeOnce(
                        continuation,
                        with: .failure(CLIBridgeError.executionFailed(exitCode: -1, message: error.localizedDescription))
                    )
                }
            }
        }

    }

    /// Executes a CLI command with stdin input and returns the result
    /// - Parameters:
    ///   - args: Arguments to pass to the CLI
    ///   - stdin: Content to write to the process's stdin
    ///   - timeout: Maximum time to wait for the command to complete
    /// - Returns: The command result including output and exit code
    func executeWithStdin(_ args: [String], stdin: String, timeout: TimeInterval = 30) async throws -> CLIBridgeResponse {
        guard let path = await cliPath() else {
            throw CLIBridgeError.notInstalled
        }

        return try await withCheckedThrowingContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: path)
            process.arguments = args

            let stdinPipe = Pipe()
            let stdoutPipe = Pipe()
            let stderrPipe = Pipe()
            process.standardInput = stdinPipe
            process.standardOutput = stdoutPipe
            process.standardError = stderrPipe

            let outputCollector = ProcessOutputCollector()
            let continuationGate = ContinuationGate()

            stdoutPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty else { return }

                Task {
                    await outputCollector.appendStdout(data)
                }
            }

            stderrPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty else { return }

                Task {
                    await outputCollector.appendStderr(data)
                }
            }

            let timeoutWorkItem = DispatchWorkItem {
                Task {
                    if process.isRunning {
                        process.terminate()
                    }

                    await continuationGate.resumeOnce(continuation, with: .failure(CLIBridgeError.timeout))
                }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timeoutWorkItem)

            process.terminationHandler = { proc in
                timeoutWorkItem.cancel()

                stdoutPipe.fileHandleForReading.readabilityHandler = nil
                stderrPipe.fileHandleForReading.readabilityHandler = nil

                Task {
                    let remainingStdout = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
                    let remainingStderr = stderrPipe.fileHandleForReading.readDataToEndOfFile()

                    if !remainingStdout.isEmpty {
                        await outputCollector.appendStdout(remainingStdout)
                    }
                    if !remainingStderr.isEmpty {
                        await outputCollector.appendStderr(remainingStderr)
                    }

                    let output = await outputCollector.finalize()
                    let response = CLIBridgeResponse(
                        success: proc.terminationStatus == 0,
                        output: output.stdout,
                        errorOutput: output.stderr,
                        exitCode: proc.terminationStatus
                    )

                    await continuationGate.resumeOnce(continuation, with: .success(response))
                }
            }

            do {
                try process.run()

                if let stdinData = stdin.data(using: .utf8) {
                    stdinPipe.fileHandleForWriting.write(stdinData)
                }
                try? stdinPipe.fileHandleForWriting.close()
            } catch {
                timeoutWorkItem.cancel()
                Task {
                    await continuationGate.resumeOnce(
                        continuation,
                        with: .failure(CLIBridgeError.executionFailed(exitCode: -1, message: error.localizedDescription))
                    )
                }
            }
        }
    }

    // MARK: - Convenience Methods

    /// Executes a command with JSON output and decodes the result
    func executeJSON<T: Decodable>(_ args: [String], as type: T.Type, timeout: TimeInterval = 30) async throws -> T {
        let response = try await execute(args, timeout: timeout)
        return try response.decode(type)
    }

    /// Executes a command expecting a standard CLIResponse structure
    /// - Parameters:
    ///   - args: Arguments to pass to the CLI
    ///   - dataType: Expected data type for successful response
    ///   - source: Source identifier for error context (e.g., "Deployer", "Database Browser")
    ///   - timeout: Maximum time to wait for the command to complete
    func executeCommand<T: Decodable>(
        _ args: [String],
        dataType: T.Type,
        source: String = "CLI",
        timeout: TimeInterval = 30
    ) async throws -> T {
        let response = try await execute(args, timeout: timeout)
        let result = try response.decodeResponse(dataType)

        guard result.success else {
            if let errorDetail = result.error {
                throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
            }
            throw CLIBridgeError.executionFailed(exitCode: 1, message: "Unknown error")
        }

        guard let data = result.data else {
            throw CLIBridgeError.invalidResponse("Success response missing data")
        }

        return data
    }

    // MARK: - Streaming (for long-running commands)

    /// Executes a command and streams output line by line
    /// Useful for commands like `logs show -f` that produce continuous output
    func executeStreaming(_ args: [String]) async -> AsyncStream<String> {
        guard let path = await cliPath() else {
            return AsyncStream { $0.finish() }
        }

        return AsyncStream { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: path)
            process.arguments = args

            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe

            pipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if data.isEmpty {
                    return
                }

                if let line = String(data: data, encoding: .utf8) {
                    continuation.yield(line)
                }
            }

            process.terminationHandler = { _ in
                pipe.fileHandleForReading.readabilityHandler = nil
                continuation.finish()
            }

            do {
                try process.run()
            } catch {
                continuation.finish()
            }

            continuation.onTermination = { @Sendable _ in
                if process.isRunning {
                    process.terminate()
                }
            }
        }
    }

    // MARK: - Project CRUD

    func projectList() async throws -> [ProjectConfiguration] {
        let response = try await execute(["project", "list", "--json"])
        let result = try response.decodeResponse([ProjectConfiguration].self)
        return result.data ?? []
    }

    func projectShow(id: String) async throws -> ProjectConfiguration {
        let response = try await execute(["project", "show", id, "--json"])
        let result = try response.decodeResponse(ProjectConfiguration.self)
        return result.data!
    }

    func projectCreate(name: String, domain: String) async throws -> ProjectConfiguration {
        let spec = [
            "name": name,
            "domain": domain
        ] as [String: Any]

        let data = try JSONSerialization.data(withJSONObject: spec)
        let jsonString = String(data: data, encoding: .utf8)!

        let response = try await execute(["project", "create", "--json", jsonString])
        let result = try response.decodeResponse(ProjectConfiguration.self)
        return result.data!
    }

    func projectSet(_ project: ProjectConfiguration) async throws -> Void {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(project)
        let jsonString = String(data: data, encoding: .utf8)!
        _ = try await execute(["project", "set", "--json", jsonString])
    }

    func projectDelete(id: String) async throws -> Void {
        _ = try await execute(["project", "delete", id])
    }

    // MARK: - Server CRUD

    func serverList() async throws -> [ServerConfig] {
        let response = try await execute(["server", "list", "--json"])
        let result = try response.decodeResponse([ServerConfig].self)
        return result.data ?? []
    }

    func serverShow(id: String) async throws -> ServerConfig {
        let response = try await execute(["server", "show", id, "--json"])
        let result = try response.decodeResponse(ServerConfig.self)
        return result.data!
    }

    func serverSet(_ server: ServerConfig) async throws -> Void {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(server)
        let jsonString = String(data: data, encoding: .utf8)!
        _ = try await execute(["server", "set", "--json", jsonString])
    }

    func serverDelete(id: String) async throws -> Void {
        _ = try await execute(["server", "delete", id])
    }

    // MARK: - Component CRUD

    func componentList() async throws -> [ComponentConfiguration] {
        let response = try await execute(["component", "list", "--json"])
        let result = try response.decodeResponse([ComponentConfiguration].self)
        return result.data ?? []
    }

    func componentShow(id: String) async throws -> ComponentConfiguration {
        let response = try await execute(["component", "show", id, "--json"])
        let result = try response.decodeResponse(ComponentConfiguration.self)
        return result.data!
    }

    func componentSet(_ component: ComponentConfiguration) async throws -> Void {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(component)
        let jsonString = String(data: data, encoding: .utf8)!
        _ = try await execute(["component", "set", "--json", jsonString])
    }

    func componentDelete(id: String) async throws -> Void {
        _ = try await execute(["component", "delete", id])
    }
}
