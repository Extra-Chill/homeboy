import SwiftUI

struct SidebarView: View {
    @Binding var selectedItem: NavigationItem?
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var moduleManager = ModuleManager.shared
    
    var body: some View {
        VStack(spacing: 0) {
            // Site Switcher Header
            ProjectSwitcherView()
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
            
            Divider()
            
            List(selection: $selectedItem) {
                // Core Tools Section
                Section("Tools") {
                    ForEach(coreTools, id: \.self) { tool in
                        Label(tool.rawValue, systemImage: tool.icon)
                            .tag(NavigationItem.coreTool(tool))
                    }
                }
                
                // Dynamic Modules Section
                if !moduleManager.modules.isEmpty {
                    Section("Modules") {
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
                                } else if module.state == .needsSetup {
                                    Image(systemName: "exclamationmark.circle.fill")
                                        .foregroundColor(.orange)
                                        .font(.caption)
                                }
                            }
                            .tag(NavigationItem.module(module.id))
                            .help(module.isDisabled ? "Requires: \(module.missingComponents.joined(separator: ", "))" : "")
                        }
                    }
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
    
    /// Core tools excluding settings (shown in separate section)
    private var coreTools: [CoreTool] {
        CoreTool.allCases.filter { $0 != .settings }
    }
}

#Preview {
    SidebarView(selectedItem: .constant(.coreTool(.deployer)))
        .environmentObject(AuthManager())
}
