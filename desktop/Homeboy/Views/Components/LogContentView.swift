import SwiftUI

/// A read-only view for displaying log file content
/// Supports monospace font, text selection, and future enhancements like error highlighting
struct LogContentView: View {
    let content: String
    let isLoading: Bool
    
    var body: some View {
        ScrollViewReader { proxy in
            ScrollView([.horizontal, .vertical]) {
                Text(content)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .id("logContent")
            }
            .background(Color(nsColor: .textBackgroundColor))
            .onChange(of: content) { _, _ in
                // Auto-scroll to bottom when content changes
                withAnimation {
                    proxy.scrollTo("logContent", anchor: .bottom)
                }
            }
        }
        .overlay {
            if isLoading {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color(nsColor: .windowBackgroundColor).opacity(0.7))
            }
        }
    }
}

/// Empty state view for when no log content is available
struct LogEmptyView: View {
    let fileName: String
    let fileExists: Bool
    
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: fileExists ? "doc.text" : "doc.questionmark")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            if fileExists {
                Text("Log file is empty")
                    .font(.headline)
                Text("\(fileName) exists but contains no content")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            } else {
                Text("\(fileName) does not exist")
                    .font(.headline)
                Text("This log file doesn't exist on the server yet")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview("With Content") {
    LogContentView(
        content: """
        [08-Jan-2026 12:34:56 UTC] PHP Notice: Undefined variable: test in /var/www/html/wp-content/themes/mytheme/functions.php on line 42
        [08-Jan-2026 12:35:01 UTC] PHP Warning: Invalid argument supplied for foreach() in /var/www/html/wp-content/plugins/myplugin/class-handler.php on line 128
        [08-Jan-2026 12:35:15 UTC] PHP Fatal error: Uncaught Error: Call to undefined function my_function() in /var/www/html/wp-content/themes/mytheme/template.php:55
        """,
        isLoading: false
    )
    .frame(width: 600, height: 300)
}

#Preview("Empty") {
    LogEmptyView(fileName: "debug.log", fileExists: true)
}
