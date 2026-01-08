import SwiftUI

/// Compact inline error display for form fields and tight spaces
struct InlineErrorView: View {
    let error: AppError
    let onDismiss: (() -> Void)?
    
    @State private var showCopied = false
    
    /// Initialize with error message and source context
    init(_ message: String, source: String, path: String? = nil, onDismiss: (() -> Void)? = nil) {
        self.error = AppError(message, source: source, path: path)
        self.onDismiss = onDismiss
    }
    
    /// Initialize with pre-built AppError
    init(_ error: AppError, onDismiss: (() -> Void)? = nil) {
        self.error = error
        self.onDismiss = onDismiss
    }
    
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.circle.fill")
                .foregroundColor(.red)
            
            Text(error.body)
                .foregroundColor(.secondary)
                .lineLimit(2)
            
            Spacer()
            
            Button(action: performCopy) {
                Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                    .foregroundColor(showCopied ? .green : .secondary)
            }
            .buttonStyle(.borderless)
            .help(showCopied ? "Copied!" : "Copy error details")
            
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
    
    private func performCopy() {
        error.copyToClipboard()
        showCopied = true
        
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            showCopied = false
        }
    }
}
