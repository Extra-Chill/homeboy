import SwiftUI

enum SidebarItem: String, CaseIterable, Identifiable {
    case bandcampScraper = "Bandcamp Scraper"
    case wpcliTerminal = "WP-CLI Terminal"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .bandcampScraper: return "music.note.list"
        case .wpcliTerminal: return "terminal"
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
    
    @ViewBuilder
    private var detailView: some View {
        switch selectedItem {
        case .bandcampScraper:
            BandcampScraperView()
        case .wpcliTerminal:
            WPCLITerminalView()
        case .settings:
            SettingsView()
        case .none:
            Text("Select an item from the sidebar")
                .foregroundColor(.secondary)
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
