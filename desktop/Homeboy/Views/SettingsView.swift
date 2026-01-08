import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject var config = ConfigurationManager.shared
    
    var body: some View {
        TabView {
            GeneralSettingsTab(config: config)
                .tabItem { Text("General") }
            
            ServersSettingsTab(config: config)
                .tabItem { Text("Servers") }
            
            DatabaseSettingsTab(config: config)
                .tabItem { Text("Database") }
            
            WordPressSettingsTab(config: config)
                .environmentObject(authManager)
                .tabItem { Text("WordPress") }
            
            ComponentsSettingsTab(config: config)
                .tabItem { Text("Components") }
            
            ModulesSettingsTab()
                .tabItem { Text("Modules") }
        }
        .padding()
        .frame(minWidth: 600, minHeight: 500)
    }
}

#Preview {
    SettingsView()
        .environmentObject(AuthManager())
}
