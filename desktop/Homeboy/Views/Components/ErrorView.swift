import SwiftUI

/// Full-screen error display with copy functionality and hint support
struct ErrorView: View {
    let error: any DisplayableError
    let onRetry: (() -> Void)?

    /// Initialize with error message and source context (creates AppError)
    init(_ message: String, source: String, path: String? = nil, onRetry: (() -> Void)? = nil) {
        self.error = AppError(message, source: source, path: path)
        self.onRetry = onRetry
    }

    /// Initialize with any DisplayableError
    init(_ error: any DisplayableError, onRetry: (() -> Void)? = nil) {
        self.error = error
        self.onRetry = onRetry
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.largeTitle)
                .foregroundColor(.orange)

            Text(error.displayMessage)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)

            if !error.displayHints.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(error.displayHints, id: \.self) { hint in
                        HStack(alignment: .top, spacing: 6) {
                            Image(systemName: "lightbulb")
                                .foregroundColor(.yellow)
                                .font(.caption)
                            Text(hint)
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    }
                }
                .padding(.horizontal)
            }

            HStack(spacing: 12) {
                CopyButton(content: error, style: .labeledError)

                if let onRetry = onRetry, error.isRetryable != false {
                    Button("Retry", action: onRetry)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
