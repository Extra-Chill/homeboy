import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var authManager: AuthManager
    @AppStorage("sendyListId") private var sendyListId = ""
    @AppStorage("localWPPath") private var localWPPath = "/Users/chubes/Developer/LocalWP/testing-grounds/app/public"
    @AppStorage("lastUsedTag") private var lastUsedTag = ""
    
    var body: some View {
        Form {
            Section("Account") {
                if let user = authManager.user {
                    LabeledContent("Logged in as", value: user.displayName)
                    LabeledContent("Email", value: user.email)
                    LabeledContent("Username", value: user.username)
                    
                    Button("Logout", role: .destructive) {
                        authManager.logout()
                    }
                } else {
                    Text("Not logged in")
                        .foregroundColor(.secondary)
                }
            }
            
            Section("Newsletter") {
                TextField("Sendy List ID", text: $sendyListId)
                    .textFieldStyle(.roundedBorder)
                Text("Enter the Sendy list ID for bulk email subscriptions")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("WP-CLI") {
                TextField("Local WP Path", text: $localWPPath)
                    .textFieldStyle(.roundedBorder)
                Text("Path to your Local WP site's public directory")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                HStack {
                    Button("Browse...") {
                        selectFolder()
                    }
                    
                    Button("Test Connection") {
                        testWPCLI()
                    }
                }
            }
            
            Section("Bandcamp Scraper") {
                TextField("Default Tag", text: $lastUsedTag)
                    .textFieldStyle(.roundedBorder)
                Text("Default Bandcamp tag to use (e.g., 'south-carolina', 'lo-fi')")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("About") {
                LabeledContent("Version", value: "0.1.0")
                LabeledContent("Build", value: "1")
            }
        }
        .formStyle(.grouped)
        .padding()
        .frame(minWidth: 500)
    }
    
    private func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Select your Local WP site's public directory"
        
        if panel.runModal() == .OK, let url = panel.url {
            localWPPath = url.path
        }
    }
    
    private func testWPCLI() {
        // TODO: Implement WP-CLI test
        print("Testing WP-CLI at: \(localWPPath)")
    }
}

#Preview {
    SettingsView()
        .environmentObject(AuthManager())
}
