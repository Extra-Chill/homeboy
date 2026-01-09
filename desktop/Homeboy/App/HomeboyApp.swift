import SwiftUI

@main
struct HomeboyApp: App {
    @StateObject private var authManager = AuthManager()

    init() {
        ConfigurationManager.shared.syncBundledProjectTypes()
        ConfigurationManager.shared.syncDocumentation()
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
