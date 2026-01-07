import SwiftUI

struct SidebarView: View {
    @Binding var selectedItem: SidebarItem?
    @EnvironmentObject var authManager: AuthManager
    
    var body: some View {
        List(selection: $selectedItem) {
            Section("Tools") {
                ForEach([SidebarItem.bandcampScraper, .cloudwaysDeployer, .wpcliTerminal], id: \.self) { item in
                    Label(item.rawValue, systemImage: item.icon)
                        .tag(item)
                }
            }
            
            Section("VPS (Coming Soon)") {
                Label("Instagram Bot", systemImage: "camera")
                    .foregroundColor(.secondary)
            }
            .disabled(true)
            
            Section {
                Label(SidebarItem.settings.rawValue, systemImage: SidebarItem.settings.icon)
                    .tag(SidebarItem.settings)
            }
        }
        .listStyle(.sidebar)
        .frame(minWidth: 200)
        .toolbar {
            ToolbarItem(placement: .automatic) {
                HStack {
                    if let user = authManager.user {
                        Text(user.displayName)
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    Button(action: { authManager.logout() }) {
                        Image(systemName: "rectangle.portrait.and.arrow.right")
                    }
                    .help("Logout")
                }
            }
        }
    }
}

#Preview {
    SidebarView(selectedItem: .constant(.bandcampScraper))
        .environmentObject(AuthManager())
}
