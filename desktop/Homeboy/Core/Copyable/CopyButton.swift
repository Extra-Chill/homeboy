import SwiftUI

/// Reusable copy button with visual feedback
struct CopyButton: View {
    let content: CopyableContent
    let style: CopyButtonStyle
    
    @State private var showCopied = false
    
    enum CopyButtonStyle {
        case icon           // Just the icon
        case labeled        // Icon + "Copy"
        case labeledError   // Icon + "Copy Error"
        case labeledWarning // Icon + "Copy Warning"
        case labeledConsole // Icon + "Copy Console"
    }
    
    init(content: CopyableContent, style: CopyButtonStyle = .labeled) {
        self.content = content
        self.style = style
    }
    
    var body: some View {
        Button(action: performCopy) {
            HStack(spacing: 4) {
                Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                
                if style != .icon {
                    Text(labelText)
                }
            }
        }
        .foregroundColor(showCopied ? .green : nil)
        .buttonStyle(.borderless)
    }
    
    private var labelText: String {
        if showCopied {
            return "Copied!"
        }
        
        switch style {
        case .icon: return ""
        case .labeled: return "Copy"
        case .labeledError: return "Copy Error"
        case .labeledWarning: return "Copy Warning"
        case .labeledConsole: return "Copy Console"
        }
    }
    
    private func performCopy() {
        content.copyToClipboard()
        showCopied = true
        
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            showCopied = false
        }
    }
}

// MARK: - Convenience Initializers

extension CopyButton {
    /// Create a copy button for an error message
    static func error(_ message: String, source: String, path: String? = nil) -> CopyButton {
        let appError = AppError(message, source: source, path: path)
        return CopyButton(content: appError, style: .labeledError)
    }
    
    /// Create a copy button for console output
    static func console(_ output: String, source: String) -> CopyButton {
        let consoleOutput = ConsoleOutput(output, source: source)
        return CopyButton(content: consoleOutput, style: .labeledConsole)
    }
    
    /// Create a copy button for a warning message
    static func warning(_ message: String, source: String) -> CopyButton {
        let appWarning = AppWarning(message, source: source)
        return CopyButton(content: appWarning, style: .labeledWarning)
    }
}
