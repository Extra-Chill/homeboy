import AppKit
import SwiftUI

/// Custom NSTextView subclass that properly responds to find panel commands
/// in a SwiftUI NSViewRepresentable context
class FindableNSTextView: NSTextView {

    /// Shows the find bar programmatically
    func showFindBar() {
        window?.makeFirstResponder(self)
        performTextFinderAction(NSTextFinder.Action.showFindInterface)
    }

    override var acceptsFirstResponder: Bool { true }

    /// Automatically become first responder when clicked
    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        super.mouseDown(with: event)
    }

    /// Ensure find panel actions are properly forwarded
    override func performFindPanelAction(_ sender: Any?) {
        if let action = sender as? NSTextFinder.Action {
            performTextFinderAction(action)
        } else {
            super.performFindPanelAction(sender)
        }
    }
}
