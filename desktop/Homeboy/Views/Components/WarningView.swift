import SwiftUI

/// Full-screen warning display with copy functionality
struct WarningView: View {
    let warning: AppWarning
    let onAction: (() -> Void)?
    let actionLabel: String
    
    @State private var showCopied = false
    
    /// Initialize with warning message and source context
    init(_ message: String, source: String, actionLabel: String = "Continue", onAction: (() -> Void)? = nil) {
        self.warning = AppWarning(message, source: source)
        self.actionLabel = actionLabel
        self.onAction = onAction
    }
    
    /// Initialize with pre-built AppWarning
    init(_ warning: AppWarning, actionLabel: String = "Continue", onAction: (() -> Void)? = nil) {
        self.warning = warning
        self.actionLabel = actionLabel
        self.onAction = onAction
    }
    
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.largeTitle)
                .foregroundColor(.orange)
            
            Text(warning.body)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
            
            HStack(spacing: 12) {
                Button(action: performCopy) {
                    HStack(spacing: 4) {
                        Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                        Text(showCopied ? "Copied!" : "Copy Warning")
                    }
                }
                .foregroundColor(showCopied ? .green : nil)
                
                if let onAction = onAction {
                    Button(actionLabel, action: onAction)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func performCopy() {
        warning.copyToClipboard()
        showCopied = true
        
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            showCopied = false
        }
    }
}
