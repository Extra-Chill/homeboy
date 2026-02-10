import SwiftUI

struct SidebarView: View {
    @Binding var selectedItem: NavigationItem?
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var moduleManager = ModuleManager.shared
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

                // Dynamic Modules Section
                Section {
                    if moduleManager.modules.isEmpty {
                        if moduleManager.isLoading {
                            Label("Loading modules...", systemImage: "arrow.trianglehead.2.clockwise")
                                .foregroundColor(.secondary)
                                .font(.callout)
                        } else {
                            Label("No modules installed", systemImage: "puzzlepiece.extension")
                                .foregroundColor(.secondary)
                                .font(.callout)
                                .onTapGesture {
                                    selectedItem = .coreTool(.settings)
                                }
                                .help("Manage modules in Settings")
                        }
                    } else {
                        ForEach(moduleManager.modules) { module in
                            HStack {
                                Label(module.name, systemImage: module.icon)
                                    .foregroundColor(module.isDisabled ? .secondary : .primary)

                                Spacer()

                                // Status indicator
                                if module.isDisabled {
                                    Image(systemName: "exclamationmark.triangle.fill")
                                        .foregroundColor(.gray)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Missing requirements: \(module.missingComponents.joined(separator: ", "))",
                                                    source: "Module: \(module.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                } else if module.state == .needsSetup {
                                    Image(systemName: "exclamationmark.circle.fill")
                                        .foregroundColor(.orange)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Setup Required",
                                                    source: "Module: \(module.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                }
                            }
                            .tag(NavigationItem.module(module.id))
                            .help(module.isDisabled ? "Requires: \(module.missingComponents.joined(separator: ", "))" : module.name)
                        }
                    }
                } header: {
                    Text("Modules")
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
