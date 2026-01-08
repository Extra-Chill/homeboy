import AppKit

/// Shared constants for NativeDataTable styling across Homeboy
enum DataTableConstants {
    static let defaultRowHeight: CGFloat = 24
    static let headerHeight: CGFloat = 28
    static let defaultMinColumnWidth: CGFloat = 80
    static let defaultIdealColumnWidth: CGFloat = 150
    static let defaultMaxColumnWidth: CGFloat = 400
    
    static let monospaceFont = NSFont.monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
    static let defaultFont = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
    
    static let primaryTextColor = NSColor.labelColor
    static let secondaryTextColor = NSColor.secondaryLabelColor
    static let nullTextColor = NSColor.tertiaryLabelColor
}
