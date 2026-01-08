import SwiftUI

/// Scrollable text view with copy button overlay for console output and logs
struct CopyableTextView: View {
    let content: CopyableContent
    let maxHeight: CGFloat?
    
    @State private var showCopied = false
    
    /// Initialize with console output
    init(console output: String, source: String, maxHeight: CGFloat? = nil) {
        self.content = ConsoleOutput(output, source: source)
        self.maxHeight = maxHeight
    }
    
    /// Initialize with any CopyableContent
    init(content: CopyableContent, maxHeight: CGFloat? = nil) {
        self.content = content
        self.maxHeight = maxHeight
    }
    
    var body: some View {
        ZStack(alignment: .topTrailing) {
            ScrollView {
                Text(content.body)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
            }
            .frame(maxHeight: maxHeight)
            .background(Color(nsColor: .textBackgroundColor))
            .cornerRadius(6)
            
            Button(action: performCopy) {
                HStack(spacing: 4) {
                    Image(systemName: showCopied ? "checkmark" : "doc.on.doc")
                    Text(showCopied ? "Copied!" : "Copy")
                        .font(.caption)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Color(nsColor: .controlBackgroundColor))
                .cornerRadius(4)
            }
            .buttonStyle(.borderless)
            .foregroundColor(showCopied ? .green : .secondary)
            .padding(8)
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
