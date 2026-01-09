import SwiftUI

/// Orientation for the split view
enum SplitOrientation {
    case horizontal  // Sidebar on left/right
    case vertical    // Panel on top/bottom
}

/// Which side contains the collapsible panel
enum CollapseSide {
    case leading   // Left (horizontal) or Top (vertical)
    case trailing  // Right (horizontal) or Bottom (vertical)
}

/// A resizable split view with a collapsible panel
///
/// Supports both horizontal (sidebar) and vertical (console/panel) orientations.
/// When collapsed, the panel shrinks to a thin strip with an expand button.
///
/// Usage:
/// ```swift
/// // Horizontal sidebar on left
/// CollapsibleSplitView(
///     orientation: .horizontal,
///     collapseSide: .leading,
///     isCollapsed: $sidebarCollapsed,
///     panelSize: (min: 200, ideal: 260, max: 400)
/// ) {
///     MainContentView()
/// } secondary: {
///     SidebarView()
/// }
///
/// // Vertical console on bottom
/// CollapsibleSplitView(
///     orientation: .vertical,
///     collapseSide: .trailing,
///     isCollapsed: $consoleCollapsed,
///     panelSize: (min: 100, ideal: 200, max: 400)
/// ) {
///     EditorView()
/// } secondary: {
///     ConsoleView()
/// }
/// ```
struct CollapsibleSplitView<Primary: View, Secondary: View>: View {
    let orientation: SplitOrientation
    let collapseSide: CollapseSide
    @Binding var isCollapsed: Bool
    
    /// Size constraints for the collapsible panel (not the main content)
    let panelSize: (min: CGFloat, ideal: CGFloat, max: CGFloat)
    
    /// Width/height of the collapsed strip
    let collapsedSize: CGFloat
    
    @ViewBuilder let primary: () -> Primary    // Main content (never collapses)
    @ViewBuilder let secondary: () -> Secondary // Collapsible panel
    
    init(
        orientation: SplitOrientation = .horizontal,
        collapseSide: CollapseSide = .leading,
        isCollapsed: Binding<Bool>,
        panelSize: (min: CGFloat, ideal: CGFloat, max: CGFloat) = (200, 260, 400),
        collapsedSize: CGFloat = 36,
        @ViewBuilder primary: @escaping () -> Primary,
        @ViewBuilder secondary: @escaping () -> Secondary
    ) {
        self.orientation = orientation
        self.collapseSide = collapseSide
        self._isCollapsed = isCollapsed
        self.panelSize = panelSize
        self.collapsedSize = collapsedSize
        self.primary = primary
        self.secondary = secondary
    }
    
    var body: some View {
        Group {
            switch orientation {
            case .horizontal:
                horizontalLayout
            case .vertical:
                verticalLayout
            }
        }
    }
    
    // MARK: - Horizontal Layout (Sidebar)
    
    @ViewBuilder
    private var horizontalLayout: some View {
        HSplitView {
            if collapseSide == .leading {
                secondaryPanel
                primary()
            } else {
                primary()
                secondaryPanel
            }
        }
    }
    
    // MARK: - Vertical Layout (Console/Panel)
    
    @ViewBuilder
    private var verticalLayout: some View {
        VSplitView {
            if collapseSide == .leading {  // Top panel
                secondaryPanel
                primary()
            } else {  // Bottom panel
                primary()
                secondaryPanel
            }
        }
    }
    
    // MARK: - Secondary Panel (Collapsible)
    
    @ViewBuilder
    private var secondaryPanel: some View {
        if isCollapsed {
            collapsedStrip
                .frame(
                    width: orientation == .horizontal ? collapsedSize : nil,
                    height: orientation == .vertical ? collapsedSize : nil
                )
        } else {
            secondary()
                .frame(
                    minWidth: orientation == .horizontal ? panelSize.min : nil,
                    idealWidth: orientation == .horizontal ? panelSize.ideal : nil,
                    maxWidth: orientation == .horizontal ? panelSize.max : nil,
                    minHeight: orientation == .vertical ? panelSize.min : nil,
                    idealHeight: orientation == .vertical ? panelSize.ideal : nil,
                    maxHeight: orientation == .vertical ? panelSize.max : nil
                )
        }
    }
    
    // MARK: - Collapsed Strip
    
    private var collapsedStrip: some View {
        let iconName: String = {
            switch (orientation, collapseSide) {
            case (.horizontal, .leading): return "sidebar.left"
            case (.horizontal, .trailing): return "sidebar.right"
            case (.vertical, .leading): return "rectangle.split.1x2"
            case (.vertical, .trailing): return "rectangle.split.1x2"
            }
        }()
        
        return Group {
            if orientation == .horizontal {
                VStack {
                    Button { isCollapsed = false } label: {
                        Image(systemName: iconName)
                            .font(.system(size: 14))
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.borderless)
                    .padding(.top, 10)
                    .help("Show sidebar")
                    Spacer()
                }
                .frame(maxWidth: .infinity)
            } else {
                HStack {
                    Button { isCollapsed = false } label: {
                        Image(systemName: iconName)
                            .font(.system(size: 14))
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.borderless)
                    .padding(.leading, 10)
                    .help("Show panel")
                    Spacer()
                }
                .frame(maxHeight: .infinity)
            }
        }
        .background(Color(nsColor: .controlBackgroundColor))
    }
}

// MARK: - Previews

#Preview("Horizontal - Left Sidebar") {
    struct PreviewWrapper: View {
        @State private var isCollapsed = false
        
        var body: some View {
            CollapsibleSplitView(
                orientation: .horizontal,
                collapseSide: .leading,
                isCollapsed: $isCollapsed,
                panelSize: (min: 200, ideal: 260, max: 400)
            ) {
                VStack {
                    Text("Main Content")
                    Button("Toggle Sidebar") { isCollapsed.toggle() }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.blue.opacity(0.1))
            } secondary: {
                VStack {
                    Text("Sidebar")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.green.opacity(0.1))
            }
            .frame(width: 800, height: 500)
        }
    }
    return PreviewWrapper()
}

#Preview("Vertical - Bottom Panel") {
    struct PreviewWrapper: View {
        @State private var isCollapsed = false
        
        var body: some View {
            CollapsibleSplitView(
                orientation: .vertical,
                collapseSide: .trailing,
                isCollapsed: $isCollapsed,
                panelSize: (min: 100, ideal: 150, max: 300)
            ) {
                VStack {
                    Text("Main Content")
                    Spacer()
                    Button("Toggle Panel") { isCollapsed.toggle() }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.blue.opacity(0.1))
            } secondary: {
                VStack {
                    Text("Bottom Panel / Console")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.orange.opacity(0.1))
            }
            .frame(width: 800, height: 500)
        }
    }
    return PreviewWrapper()
}
