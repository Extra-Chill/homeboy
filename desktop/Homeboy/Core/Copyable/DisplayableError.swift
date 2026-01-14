import Foundation

/// Protocol for errors that can be displayed in the UI.
/// Unifies CLI errors (with hints and retryable) and app-native errors.
protocol DisplayableError: CopyableContent {
    var displayMessage: String { get }
    var displayHints: [String] { get }
    var isRetryable: Bool? { get }
}

// MARK: - CLIError Conformance

extension CLIError: DisplayableError {
    var displayMessage: String { message }
    var displayHints: [String] { hints.map { $0.message } }
    var isRetryable: Bool? { retryable }
}

// MARK: - AppError Conformance

extension AppError: DisplayableError {
    var displayMessage: String { body }
    var displayHints: [String] { [] }
    var isRetryable: Bool? { nil }
}

// MARK: - Error Conversion Helper

extension Error {
    /// Convert any error to a DisplayableError for UI presentation.
    /// Extracts CLIError from CLIBridgeError if present, otherwise wraps in AppError.
    func toDisplayableError(source: String, path: String? = nil) -> any DisplayableError {
        if let bridgeError = self as? CLIBridgeError,
           let cliError = bridgeError.cliError {
            return cliError
        }
        return AppError(self.localizedDescription, source: source, path: path)
    }
}
