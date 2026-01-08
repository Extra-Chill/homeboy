import SwiftUI

/// Full-screen error display with copy functionality
struct ErrorView: View {
    let error: AppError
    let onRetry: (() -> Void)?
    
    @State private var showCopied = false
    
    /// Initialize with error message and source context
    init(_ message: String, source: String, path: String? = nil, onRetry: (() -> Void)? = nil) {
        self.error = AppError(message, source: source, path: path)
        self.onRetry = onRetry
    }
    
    /// Initialize with pre-built AppError
    init(_ error: AppError, onRetry: (() -> Void)? = nil) {
        self.error = error
        self.onRetry = onRetry
    }
    
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.largeTitle)
                .foregroundColor(.orange)
            
            Text(error.body)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            
            HStack(spacing: 12) {
                Button(action: performCopy) {
                    HStack(spacing: 4) {
                        Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                        Text(showCopied ? "Copied!" : "Copy Error")
                    }
                }
                .foregroundColor(showCopied ? .green : nil)
                
                if let onRetry = onRetry {
                    Button("Retry", action: onRetry)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func performCopy() {
        error.copyToClipboard()
        showCopied = true
        
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            showCopied = false
        }
    }
}
