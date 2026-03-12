import SwiftUI

struct SidebarView: View {
    @Binding var selectedItem: NavigationItem?
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var extensionManager = ExtensionManager.shared
    @ObservedObject private var config = ConfigurationManager.shared
    
    var body: some View {
        VStack(spacing: 0) {
            // Site Switcher Header
            ProjectSwitcherView()
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
            
            Divider()
            
            List(selection: $selectedItem) {
                // Core Tools Section
                Section {
                    ForEach(coreTools, id: \.self) { tool in
                        Label(tool.rawValue, systemImage: tool.icon)
                            .tag(NavigationItem.coreTool(tool))
                    }
                } header: {
                    Text("Tools")
                } footer: {
                    Text("Built-in server and deployment tools")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }

                // Dynamic Extensions Section
                Section {
                    if extensionManager.extensions.isEmpty {
                        if extensionManager.isLoading {
                            Label("Loading extensions...", systemImage: "arrow.trianglehead.2.clockwise")
                                .foregroundColor(.secondary)
                                .font(.callout)
                        } else {
                            Label("No extensions installed", systemImage: "puzzlepiece.extension")
                                .foregroundColor(.secondary)
                                .font(.callout)
                                .onTapGesture {
                                    selectedItem = .coreTool(.settings)
                                }
                                .help("Manage extensions in Settings")
                        }
                    } else {
                        ForEach(extensionManager.extensions) { extension in
                            HStack {
                                Label(extension.name, systemImage: extension.icon)
                                    .foregroundColor(extension.isDisabled ? .secondary : .primary)

                                Spacer()

                                // Status indicator
                                if extension.isDisabled {
                                    Image(systemName: "exclamationmark.triangle.fill")
                                        .foregroundColor(.gray)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Missing requirements: \(extension.missingComponents.joined(separator: ", "))",
                                                    source: "Extension: \(extension.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                } else if extension.state == .needsSetup {
                                    Image(systemName: "exclamationmark.circle.fill")
                                        .foregroundColor(.orange)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Setup Required",
                                                    source: "Extension: \(extension.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                }
                            }
                            .tag(NavigationItem.extension(extension.id))
                            .help(extension.isDisabled ? "Requires: \(extension.missingComponents.joined(separator: ", "))" : extension.name)
                        }
                    }
                } header: {
                    Text("Extensions")
                } footer: {
                    Text("Installable extensions \u{2014} manage in Settings")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                
                // Settings (separate section at bottom)
                Section {
                    Label(CoreTool.settings.rawValue, systemImage: CoreTool.settings.icon)
                        .tag(NavigationItem.coreTool(.settings))
                }
            }
            .listStyle(.sidebar)
        }
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
    
    /// Core tools to display in the sidebar.
    /// All tools are universal - Database Browser shows config prompt if not configured.
    /// Settings is shown in a separate section below.
    private var coreTools: [CoreTool] {
        CoreTool.allCases.filter { $0 != .settings }
    }
}

#Preview {
    SidebarView(selectedItem: .constant(.coreTool(.deployer)))
        .environmentObject(AuthManager())
}
