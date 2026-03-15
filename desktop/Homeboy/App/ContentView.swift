import SwiftUI

/// Navigation items: core tools are static, extensions are dynamic
enum NavigationItem: Hashable {
    case coreTool(CoreTool)
    case extensionItem(String) // Extension ID
}

/// Built-in core tools (not extensions)
/// Order: Deployer, File Editor, Log Viewer are universal.
/// Database Browser is shown if project type supports it.
/// Settings is shown in a separate section.
enum CoreTool: String, CaseIterable, Identifiable {
    case deployer = "Deployer"
    case remoteFileEditor = "File Editor"
    case remoteLogViewer = "Log Viewer"
    case databaseBrowser = "Database"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .deployer: return "arrow.up.to.line"
        case .remoteFileEditor: return "doc.badge.gearshape"
        case .remoteLogViewer: return "doc.text.magnifyingglass"
        case .databaseBrowser: return "cylinder.split.1x2"
        case .settings: return "gear"
        }
    }
}

struct ContentView: View {
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var configManager = ConfigurationManager.shared
    @ObservedObject private var extensionManager = ExtensionManager.shared
    @State private var selectedItem: NavigationItem? = .coreTool(.deployer)
    
    var body: some View {
        Group {
            if configManager.activeProject != nil {
                NavigationSplitView {
                    SidebarView(selectedItem: $selectedItem)
                } detail: {
                    detailView
                }
            } else {
                // Placeholder while waiting for project creation sheet
                Color.clear
            }
        }
        .sheet(isPresented: $configManager.needsProjectCreation) {
            CreateProjectSheet(isFirstProject: true)
        }
    }
    
    /// Views are kept mounted in a ZStack to preserve state (including running processes)
    /// when switching tabs. Only the selected view is visible via opacity.
    @ViewBuilder
    private var detailView: some View {
        ZStack {
            // Core tools - kept mounted to preserve state
            DeployerView()
                .opacity(selectedItem == .coreTool(.deployer) ? 1 : 0)
            DatabaseBrowserView()
                .opacity(selectedItem == .coreTool(.databaseBrowser) ? 1 : 0)
            RemoteLogViewerView()
                .opacity(selectedItem == .coreTool(.remoteLogViewer) ? 1 : 0)
            RemoteFileEditorView()
                .opacity(selectedItem == .coreTool(.remoteFileEditor) ? 1 : 0)
            SettingsView()
                .opacity(selectedItem == .coreTool(.settings) ? 1 : 0)
            
        // Dynamic extensions
        ForEach(extensionManager.extensions) { ext in
            ExtensionContainerView(extensionId: ext.id)
                .opacity(selectedItem == .extensionItem(ext.id) ? 1 : 0)
        }
            
            // Empty state
            if selectedItem == nil {
                ContentUnavailableView(
                    "Select an Item",
                    systemImage: "sidebar.left",
                    description: Text("Choose a tool or extension from the sidebar")
                )
            }
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
