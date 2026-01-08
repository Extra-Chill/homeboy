import SwiftUI

enum SidebarItem: String, CaseIterable, Identifiable {
    case bandcampScraper = "Bandcamp Scraper"
    case cloudwaysDeployer = "Cloudways Deployer"
    case wpcliTerminal = "WP-CLI Terminal"
    case databaseBrowser = "Database"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .bandcampScraper: return "music.note.list"
        case .cloudwaysDeployer: return "arrow.up.to.line"
        case .wpcliTerminal: return "terminal"
        case .databaseBrowser: return "cylinder.split.1x2"
        case .settings: return "gear"
        }
    }
}

struct ContentView: View {
    @EnvironmentObject var authManager: AuthManager
    @State private var selectedItem: SidebarItem? = .bandcampScraper
    
    var body: some View {
        Group {
            if authManager.isLoading {
                ProgressView("Loading...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if !authManager.isAuthenticated {
                LoginView()
            } else {
                NavigationSplitView {
                    SidebarView(selectedItem: $selectedItem)
                } detail: {
                    detailView
                }
            }
        }
    }
    
    /// Views are kept mounted in a ZStack to preserve state (including running processes)
    /// when switching tabs. Only the selected view is visible via opacity.
    private var detailView: some View {
        ZStack {
            BandcampScraperView()
                .opacity(selectedItem == .bandcampScraper ? 1 : 0)
            CloudwaysDeployerView()
                .opacity(selectedItem == .cloudwaysDeployer ? 1 : 0)
            WPCLITerminalView()
                .opacity(selectedItem == .wpcliTerminal ? 1 : 0)
            DatabaseBrowserView()
                .opacity(selectedItem == .databaseBrowser ? 1 : 0)
            SettingsView()
                .opacity(selectedItem == .settings ? 1 : 0)
            
            if selectedItem == nil {
                Text("Select an item from the sidebar")
                    .foregroundColor(.secondary)
            }
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
