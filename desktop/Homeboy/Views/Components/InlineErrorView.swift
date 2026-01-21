import SwiftUI

/// Compact inline error display for form fields and tight spaces
struct InlineErrorView: View {
    let error: any DisplayableError
    let onDismiss: (() -> Void)?

    /// Initialize with error message and source context (creates AppError)
    init(_ message: String, source: String, path: String? = nil, onDismiss: (() -> Void)? = nil) {
        self.error = AppError(message, source: source, path: path)
        self.onDismiss = onDismiss
    }

    /// Initialize with any DisplayableError
    init(_ error: any DisplayableError, onDismiss: (() -> Void)? = nil) {
        self.error = error
        self.onDismiss = onDismiss
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.circle.fill")
                .foregroundColor(.red)

            VStack(alignment: .leading, spacing: 2) {
                Text(error.displayMessage)
                    .foregroundColor(.secondary)
                    .lineLimit(2)

                if let firstHint = error.displayHints.first {
                    Text(firstHint)
                        .font(.caption)
                        .foregroundColor(.secondary.opacity(0.8))
                        .lineLimit(1)
                }
            }

            Spacer()

            if error.isRetryable == true {
                Text("Retryable")
                    .font(.caption2)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.blue.opacity(0.2))
                    .cornerRadius(4)
            }

            CopyButton(content: error, style: .icon)

            if let onDismiss = onDismiss {
                Button(action: onDismiss) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(8)
        .background(Color.red.opacity(0.1))
        .cornerRadius(6)
    }
}
