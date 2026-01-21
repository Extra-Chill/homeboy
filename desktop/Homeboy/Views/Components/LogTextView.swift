import SwiftUI
import AppKit

/// NSTextView wrapper with native macOS find bar support for read-only log content
struct LogTextView: NSViewRepresentable {
    let content: String
    let isLoading: Bool

    func makeNSView(context: Context) -> NSScrollView {
        let textView = FindableNSTextView()

        // Text styling
        textView.font = .monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        textView.textColor = .textColor

        // Read-only behavior
        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.importsGraphics = false

        // Find bar (Cmd+F) - system handles this automatically
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true

        // Background styling
        textView.drawsBackground = true
        textView.backgroundColor = .textBackgroundColor

        // Layout - allow horizontal scrolling for long lines
        textView.isHorizontallyResizable = true
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width, .height]
        textView.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = false
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)

        // Set initial content
        textView.string = content

        // Wrap in scroll view
        let scrollView = NSScrollView()
        scrollView.documentView = textView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = false

        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let textView = scrollView.documentView as? NSTextView else { return }

        // Only update if content differs to avoid cursor jumping
        if textView.string != content {
            let wasAtBottom = textView.visibleRect.maxY >= textView.bounds.height - 10 // Close to bottom
            textView.string = content

            // Auto-scroll to bottom if content changed and we were at the bottom
            if wasAtBottom {
                textView.scrollToEndOfDocument(nil)
            }
        }
    }
}