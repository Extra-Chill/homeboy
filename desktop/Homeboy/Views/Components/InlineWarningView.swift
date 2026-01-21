import SwiftUI

/// Compact inline warning display for form fields and tight spaces
struct InlineWarningView: View {
    let warning: AppWarning
    let onAction: (() -> Void)?
    let actionLabel: String?

    /// Initialize with warning message and source context
    init(_ message: String, source: String, actionLabel: String? = nil, onAction: (() -> Void)? = nil) {
        self.warning = AppWarning(message, source: source)
        self.actionLabel = actionLabel
        self.onAction = onAction
    }
    
    /// Initialize with pre-built AppWarning
    init(_ warning: AppWarning, actionLabel: String? = nil, onAction: (() -> Void)? = nil) {
        self.warning = warning
        self.actionLabel = actionLabel
        self.onAction = onAction
    }
    
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            
            Text(warning.body)
                .foregroundColor(.secondary)
                .lineLimit(2)
            
            Spacer()

            CopyButton(content: warning, style: .icon)

            if let onAction = onAction, let label = actionLabel {
                Button(label, action: onAction)
                    .buttonStyle(.borderless)
            }
        }
        .padding(8)
        .background(Color.orange.opacity(0.1))
        .cornerRadius(6)
    }
}
