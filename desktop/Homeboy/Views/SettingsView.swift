import SwiftUI

struct SettingsView: View {
    @ObservedObject var config = ConfigurationManager.shared
    
    var body: some View {
        TabView {
            GeneralSettingsTab(config: config)
                .tabItem { Text("General") }
            
            ServersSettingsTab(config: config)
                .tabItem { Text("Servers") }
            
            if config.safeActiveProject.isWordPress {
                WordPressSettingsTab(config: config)
                    .tabItem { Text("WordPress") }
            }
            
            ComponentsSettingsTab(config: config)
                .tabItem { Text("Components") }
            
            ExtensionsSettingsTab()
                .tabItem { Text("Extensions") }
        }
        .padding()
        .frame(minWidth: 600, minHeight: 500)
    }
}

#Preview {
    SettingsView()
}
