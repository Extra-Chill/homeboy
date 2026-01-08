import SwiftUI

/// Navigation items: core tools are static, modules are dynamic
enum NavigationItem: Hashable {
    case coreTool(CoreTool)
    case module(String)  // Module ID
}

/// Built-in core tools (not modules)
enum CoreTool: String, CaseIterable, Identifiable {
    case deployer = "Deployer"
    case wpcliTerminal = "WP-CLI Terminal"
    case databaseBrowser = "Database"
    case debugLogs = "Debug Logs"
    case configEditor = "Config Editor"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .deployer: return "arrow.up.to.line"
        case .wpcliTerminal: return "terminal"
        case .databaseBrowser: return "cylinder.split.1x2"
        case .debugLogs: return "doc.text.magnifyingglass"
        case .configEditor: return "doc.badge.gearshape"
        case .settings: return "gear"
        }
    }
}

struct ContentView: View {
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var moduleManager = ModuleManager.shared
    @State private var selectedItem: NavigationItem? = .coreTool(.deployer)
    
    var body: some View {
        NavigationSplitView {
            SidebarView(selectedItem: $selectedItem)
        } detail: {
            detailView
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
            WPCLITerminalView()
                .opacity(selectedItem == .coreTool(.wpcliTerminal) ? 1 : 0)
            DatabaseBrowserView()
                .opacity(selectedItem == .coreTool(.databaseBrowser) ? 1 : 0)
            DebugLogsView()
                .opacity(selectedItem == .coreTool(.debugLogs) ? 1 : 0)
            ConfigEditorView()
                .opacity(selectedItem == .coreTool(.configEditor) ? 1 : 0)
            SettingsView()
                .opacity(selectedItem == .coreTool(.settings) ? 1 : 0)
            
            // Dynamic modules
            ForEach(moduleManager.modules) { module in
                ModuleContainerView(moduleId: module.id)
                    .opacity(selectedItem == .module(module.id) ? 1 : 0)
            }
            
            // Empty state
            if selectedItem == nil {
                ContentUnavailableView(
                    "Select an Item",
                    systemImage: "sidebar.left",
                    description: Text("Choose a tool or module from the sidebar")
                )
            }
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
