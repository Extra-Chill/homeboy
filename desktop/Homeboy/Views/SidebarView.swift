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
                        ForEach(extensionManager.extensions) { loadedExtension in
                            HStack {
                                Label(loadedExtension.name, systemImage: loadedExtension.icon)
                                    .foregroundColor(loadedExtension.isDisabled ? .secondary : .primary)

                                Spacer()

                                // Status indicator
                                if loadedExtension.isDisabled {
                                    Image(systemName: "exclamationmark.triangle.fill")
                                        .foregroundColor(.gray)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Missing requirements: \(loadedExtension.missingComponents.joined(separator: ", "))",
                                                    source: "Extension: \(loadedExtension.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                } else if loadedExtension.state == .needsSetup {
                                    Image(systemName: "exclamationmark.circle.fill")
                                        .foregroundColor(.orange)
                                        .font(.caption)
                                        .contextMenu {
                                            Button("Copy Warning") {
                                                AppWarning(
                                                    "Setup Required",
                                                    source: "Extension: \(loadedExtension.name)"
                                                ).copyToClipboard()
                                            }
                                        }
                                }
                            }
                            .tag(NavigationItem.extensionItem(loadedExtension.id))
                            .help(loadedExtension.isDisabled ? "Requires: \(loadedExtension.missingComponents.joined(separator: ", "))" : loadedExtension.name)
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
        .onChange(of: config.activeProject?.id) { _, _ in
            ensureSelectionIsVisible()
        }
        .onChange(of: extensionManager.extensions.map(\.id)) { _, _ in
            ensureSelectionIsVisible()
        }
    }

    private func ensureSelectionIsVisible() {
        switch selectedItem {
        case .coreTool(let tool):
            if !coreTools.contains(tool), tool != .settings {
                selectedItem = coreTools.first.map(NavigationItem.coreTool) ?? .coreTool(.settings)
            }
        case .extensionItem(let id):
            if !extensionManager.extensions.contains(where: { $0.id == id }) {
                selectedItem = coreTools.first.map(NavigationItem.coreTool) ?? .coreTool(.settings)
            }
        case nil:
            selectedItem = coreTools.first.map(NavigationItem.coreTool) ?? .coreTool(.settings)
        }
    }
    
    /// Core tools to display in the sidebar.
    /// All tools are universal - Database Browser shows config prompt if not configured.
    /// Settings is shown in a separate section below.
    private var coreTools: [CoreTool] {
        CoreTool.allCases.filter { $0 != .settings && $0.isAvailable(for: config.activeProject) }
    }
}

#Preview {
    SidebarView(selectedItem: .constant(.coreTool(.deployer)))
        .environmentObject(AuthManager())
}
