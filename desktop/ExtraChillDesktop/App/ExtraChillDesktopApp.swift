import SwiftUI

@main
struct ExtraChillDesktopApp: App {
    @StateObject private var authManager = AuthManager()
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(authManager)
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1000, height: 700)
        
        Settings {
            SettingsView()
                .environmentObject(authManager)
        }
    }
}
