import SwiftUI

@main
struct HomeboyApp: App {
    @StateObject private var authManager = AuthManager()
    
    init() {
        // Run migration from ExtraChillDesktop to Homeboy on first launch
        MigrationService.migrateIfNeeded()
    }
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(authManager)
                .onAppear {
                    CLIInstaller.shared.promptInstallIfNeeded()
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1000, height: 700)
        
        Settings {
            SettingsView()
                .environmentObject(authManager)
        }
    }
}
