import Foundation

// Note: `CLIBridge` is defined in `Homeboy/Core/CLI/CLIBridge.swift`.

@MainActor
final class HomeboyCLI {
    static let shared = HomeboyCLI()

    let cli = CLIBridge.shared

    var isInstalled: Bool {
        cli.isInstalled
    }

    private init() {}

    private static var decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    func executeCommandWithOutputFile<T: Decodable>(
        _ args: [String],
        dataType: T.Type,
        source: String,
        timeout: TimeInterval = 120
    ) async throws -> T {
        let outputURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("homeboy-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: outputURL) }

        var outputArgs = args
        outputArgs.append(contentsOf: ["--output", outputURL.path])

        let response = try await cli.execute(outputArgs, timeout: timeout)
        guard FileManager.default.fileExists(atPath: outputURL.path) else {
            if response.success {
                throw CLIBridgeError.invalidResponse("Missing output file for \(source)")
            }
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }

        let data = try Data(contentsOf: outputURL)
        let result = try Self.decoder.decode(CLIBridgeResult<T>.self, from: data)
        guard result.success else {
            if let errorDetail = result.error {
                throw CLIBridgeError.cliError(errorDetail.toCLIError(source: source))
            }
            throw CLIBridgeError.executionFailed(exitCode: response.exitCode, message: response.errorOutput)
        }
        guard let decoded = result.data else {
            throw CLIBridgeError.invalidResponse("Success response missing data")
        }

        return decoded
    }
}
