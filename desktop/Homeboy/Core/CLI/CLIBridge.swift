import Foundation

/// Response structure for CLI execution results
struct CLIBridgeResponse: Sendable {
    let success: Bool
    let output: String
    let errorOutput: String
    let exitCode: Int32

    /// Attempts to decode the output as a JSON response with the given data type
    func decode<T: Decodable>(_ type: T.Type) throws -> T {
        guard success else {
            throw CLIBridgeError.executionFailed(exitCode: exitCode, message: errorOutput)
        }

        guard let data = output.data(using: .utf8) else {
            throw CLIBridgeError.invalidResponse("Output is not valid UTF-8")
        }

        return try JSONDecoder().decode(type, from: data)
    }

    /// Attempts to decode as a standard CLIResponse structure
    func decodeResponse<T: Decodable>(_ dataType: T.Type) throws -> CLIBridgeResult<T> {
        guard let data = output.data(using: .utf8) else {
            throw CLIBridgeError.invalidResponse("Output is not valid UTF-8")
        }

        return try JSONDecoder().decode(CLIBridgeResult<T>.self, from: data)
    }
}

/// Standard response structure matching CLI output contract
struct CLIBridgeResult<T: Decodable>: Decodable {
    let success: Bool
    let data: T?
    let error: CLIBridgeErrorDetail?
}

/// Error detail from CLI response
struct CLIBridgeErrorDetail: Decodable {
    let code: String
    let message: String
}

/// Errors that can occur during CLI bridge operations
enum CLIBridgeError: LocalizedError {
    case notInstalled
    case executionFailed(exitCode: Int32, message: String)
    case invalidResponse(String)
    case timeout

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
        }
    }
}

/// Bridge for executing CLI commands from the GUI.
/// This allows the GUI to shell out to the CLI binary instead of importing Core/ services directly.
actor CLIBridge {
    static let shared = CLIBridge()

    private init() {}

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

            var stdoutData = Data()
            var stderrData = Data()

            // Read stdout
            stdoutPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    stdoutData.append(data)
                }
            }

            // Read stderr
            stderrPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    stderrData.append(data)
                }
            }

            // Timeout handling
            let timeoutWorkItem = DispatchWorkItem {
                if process.isRunning {
                    process.terminate()
                }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timeoutWorkItem)

            process.terminationHandler = { proc in
                timeoutWorkItem.cancel()

                // Clean up handlers
                stdoutPipe.fileHandleForReading.readabilityHandler = nil
                stderrPipe.fileHandleForReading.readabilityHandler = nil

                // Get any remaining data
                stdoutData.append(stdoutPipe.fileHandleForReading.readDataToEndOfFile())
                stderrData.append(stderrPipe.fileHandleForReading.readDataToEndOfFile())

                let stdout = String(data: stdoutData, encoding: .utf8) ?? ""
                let stderr = String(data: stderrData, encoding: .utf8) ?? ""

                let response = CLIBridgeResponse(
                    success: proc.terminationStatus == 0,
                    output: stdout,
                    errorOutput: stderr,
                    exitCode: proc.terminationStatus
                )

                continuation.resume(returning: response)
            }

            do {
                try process.run()
            } catch {
                timeoutWorkItem.cancel()
                continuation.resume(throwing: CLIBridgeError.executionFailed(exitCode: -1, message: error.localizedDescription))
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

            var stdoutData = Data()
            var stderrData = Data()

            // Read stdout
            stdoutPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    stdoutData.append(data)
                }
            }

            // Read stderr
            stderrPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if !data.isEmpty {
                    stderrData.append(data)
                }
            }

            // Timeout handling
            let timeoutWorkItem = DispatchWorkItem {
                if process.isRunning {
                    process.terminate()
                }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timeoutWorkItem)

            process.terminationHandler = { proc in
                timeoutWorkItem.cancel()

                // Clean up handlers
                stdoutPipe.fileHandleForReading.readabilityHandler = nil
                stderrPipe.fileHandleForReading.readabilityHandler = nil

                // Get any remaining data
                stdoutData.append(stdoutPipe.fileHandleForReading.readDataToEndOfFile())
                stderrData.append(stderrPipe.fileHandleForReading.readDataToEndOfFile())

                let stdout = String(data: stdoutData, encoding: .utf8) ?? ""
                let stderr = String(data: stderrData, encoding: .utf8) ?? ""

                let response = CLIBridgeResponse(
                    success: proc.terminationStatus == 0,
                    output: stdout,
                    errorOutput: stderr,
                    exitCode: proc.terminationStatus
                )

                continuation.resume(returning: response)
            }

            do {
                try process.run()

                // Write stdin content and close
                if let stdinData = stdin.data(using: .utf8) {
                    stdinPipe.fileHandleForWriting.write(stdinData)
                }
                stdinPipe.fileHandleForWriting.closeFile()
            } catch {
                timeoutWorkItem.cancel()
                continuation.resume(throwing: CLIBridgeError.executionFailed(exitCode: -1, message: error.localizedDescription))
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
    func executeCommand<T: Decodable>(_ args: [String], dataType: T.Type, timeout: TimeInterval = 30) async throws -> T {
        let response = try await execute(args, timeout: timeout)
        let result = try response.decodeResponse(dataType)

        guard result.success else {
            let message = result.error?.message ?? "Unknown error"
            let code = result.error?.code ?? "UNKNOWN"
            throw CLIBridgeError.executionFailed(exitCode: 1, message: "[\(code)] \(message)")
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
}
