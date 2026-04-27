import Combine
import SwiftUI

/// Navigation items: core tools are static, extensions are dynamic
enum NavigationItem: Hashable {
    case coreTool(CoreTool)
    case extensionItem(String) // Extension ID
}

/// Built-in core tools (not extensions)
/// Order: Deployer, File Editor, Log Viewer are universal.
/// Database Browser is shown if project type supports it.
/// Settings is shown in a separate section.
enum CoreTool: String, CaseIterable, Identifiable {
    case deployer = "Deployer"
    case bench = "Bench"
    case release = "Release"
    case remoteFileEditor = "File Editor"
    case remoteLogViewer = "Log Viewer"
    case databaseBrowser = "Database"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .deployer: return "arrow.up.to.line"
        case .bench: return "speedometer"
        case .release: return "tag"
        case .remoteFileEditor: return "doc.badge.gearshape"
        case .remoteLogViewer: return "doc.text.magnifyingglass"
        case .databaseBrowser: return "cylinder.split.1x2"
        case .settings: return "gear"
        }
    }
}

struct ContentView: View {
    @EnvironmentObject var authManager: AuthManager
    @ObservedObject private var configManager = ConfigurationManager.shared
    @ObservedObject private var extensionManager = ExtensionManager.shared
    @State private var selectedItem: NavigationItem? = .coreTool(.deployer)
    
    var body: some View {
        Group {
            if configManager.activeProject != nil {
                NavigationSplitView {
                    SidebarView(selectedItem: $selectedItem)
                } detail: {
                    detailView
                }
            } else {
                // Placeholder while waiting for project creation sheet
                Color.clear
            }
        }
        .sheet(isPresented: $configManager.needsProjectCreation) {
            CreateProjectSheet(isFirstProject: true)
        }
    }
    
    /// Views are kept mounted in a ZStack to preserve state (including running processes)
    /// when switching tabs. Only the selected view is visible via opacity.
    @ViewBuilder
    private var detailView: some View {
        ZStack {
            // Core tools - kept mounted to preserve state
            DeployerView()
                .opacity(selectedItem == .coreTool(.deployer) ? 1 : 0)
            BenchView()
                .opacity(selectedItem == .coreTool(.bench) ? 1 : 0)
            ReleaseWorkflowView()
                .opacity(selectedItem == .coreTool(.release) ? 1 : 0)
            DatabaseBrowserView()
                .opacity(selectedItem == .coreTool(.databaseBrowser) ? 1 : 0)
            RemoteLogViewerView()
                .opacity(selectedItem == .coreTool(.remoteLogViewer) ? 1 : 0)
            RemoteFileEditorView()
                .opacity(selectedItem == .coreTool(.remoteFileEditor) ? 1 : 0)
            SettingsView()
                .opacity(selectedItem == .coreTool(.settings) ? 1 : 0)
            
        // Dynamic extensions
        ForEach(extensionManager.extensions) { ext in
            ExtensionContainerView(extensionId: ext.id)
                .opacity(selectedItem == .extensionItem(ext.id) ? 1 : 0)
        }
            
            // Empty state
            if selectedItem == nil {
                ContentUnavailableView(
                    "Select an Item",
                    systemImage: "sidebar.left",
                    description: Text("Choose a tool or extension from the sidebar")
                )
            }
        }
    }
}

// MARK: - Release Workflow

@MainActor
final class ReleaseWorkflowViewModel: ObservableObject, ConfigurationObserving {
    var cancellables = Set<AnyCancellable>()

    @Published var components: [DeployableComponent] = []
    @Published var selectedComponentId: String?
    @Published var changes: ChangesOutput?
    @Published var version: VersionShowOutput?
    @Published var releasePlan: ReleaseOutput?
    @Published var consoleOutput = ""
    @Published var isLoading = false
    @Published var error: (any DisplayableError)?

    private let cli = HomeboyCLI.shared

    init() {
        loadComponents()
        observeConfiguration()
    }

    var selectedComponent: DeployableComponent? {
        components.first { $0.id == selectedComponentId }
    }

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch, .projectModified:
            loadComponents()
        default:
            break
        }
    }

    func loadComponents() {
        let project = ConfigurationManager.shared.activeProject ?? ConfigurationManager.shared.safeActiveProject
        components = ConfigurationManager.shared.loadComponentsForProject(project).map { DeployableComponent(from: $0) }
        if selectedComponentId == nil || !components.contains(where: { $0.id == selectedComponentId }) {
            selectedComponentId = components.first?.id
        }
        changes = nil
        version = nil
        releasePlan = nil
    }

    func refreshPlan() {
        guard let component = selectedComponent else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Release")
            return
        }

        isLoading = true
        error = nil
        changes = nil
        version = nil
        releasePlan = nil
        consoleOutput = "> homeboy changes \(component.id)\n"
        consoleOutput += "> homeboy version show \(component.id) --path \(component.localPath)\n"
        consoleOutput += "> homeboy release \(component.id) --path \(component.localPath) --dry-run\n\n"

        Task {
            do {
                async let changesResult = cli.changes(componentId: component.id)
                async let versionResult: VersionShowOutput? = try? cli.versionShow(componentId: component.id, path: component.localPath)
                async let releaseResult = cli.releaseDryRun(componentId: component.id, path: component.localPath)

                changes = try await changesResult
                version = await versionResult
                releasePlan = try await releaseResult
                appendPlanSummary()
            } catch {
                self.error = AppError(error.localizedDescription, source: "Release")
                consoleOutput += "Failed: \(error.localizedDescription)\n"
            }
            isLoading = false
        }
    }

    func runBuild() {
        guard let component = selectedComponent else { return }
        guard cli.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Release")
            return
        }

        isLoading = true
        error = nil
        consoleOutput += "\n> homeboy build \(component.id) --path \(component.localPath)\n"

        Task {
            do {
                _ = try await cli.build(componentId: component.id, path: component.localPath)
                consoleOutput += "Build completed. Refreshing release plan...\n"
                isLoading = false
                refreshPlan()
            } catch {
                self.error = AppError(error.localizedDescription, source: "Build")
                consoleOutput += "Build failed: \(error.localizedDescription)\n"
                isLoading = false
            }
        }
    }

    private func appendPlanSummary() {
        if let changes {
            consoleOutput += "Changes baseline: \(changes.baselineRef ?? "unknown")\n"
            consoleOutput += "Commits since baseline: \(changes.commits.count)\n"
            if changes.uncommitted?.hasChanges == true {
                consoleOutput += "Working tree has uncommitted changes.\n"
            }
        }
        if let version = version?.version {
            consoleOutput += "Current version: \(version)\n"
        } else {
            consoleOutput += "Current version: unavailable. Check version target configuration.\n"
        }
        if let result = releasePlan?.result {
            consoleOutput += "Release dry-run: \(result.bumpType ?? "unknown")"
            if let next = result.nextVersion {
                consoleOutput += " -> \(next)"
            }
            if let skipped = result.skippedReason {
                consoleOutput += " (skipped: \(skipped))"
            }
            consoleOutput += "\n"
        }
    }
}

struct ReleaseWorkflowView: View {
    @StateObject private var viewModel = ReleaseWorkflowViewModel()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if viewModel.components.isEmpty {
                ContentUnavailableView(
                    "No Components",
                    systemImage: "shippingbox",
                    description: Text("Add components in project settings before planning a release.")
                )
            } else {
                HSplitView {
                    planningSection
                        .frame(minWidth: 380)
                    consoleSection
                        .frame(minWidth: 320)
                }
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            if viewModel.changes == nil && !viewModel.components.isEmpty {
                viewModel.refreshPlan()
            }
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            Picker("Component", selection: $viewModel.selectedComponentId) {
                ForEach(viewModel.components) { component in
                    Text(component.name).tag(Optional(component.id))
                }
            }
            .frame(width: 240)

            Button("Refresh Plan") {
                viewModel.refreshPlan()
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.selectedComponentId == nil || viewModel.isLoading)

            Button("Build") {
                viewModel.runBuild()
            }
            .buttonStyle(.bordered)
            .disabled(viewModel.selectedComponentId == nil || viewModel.isLoading)

            Spacer()

            if viewModel.isLoading {
                ProgressView()
                    .controlSize(.small)
                Text("Running CLI...")
                    .foregroundColor(.secondary)
            }
        }
        .padding()
    }

    private var planningSection: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                summaryCard
                commitsCard
                releaseGuardCard
                if let error = viewModel.error {
                    InlineErrorView(error)
                }
            }
            .padding()
        }
    }

    private var summaryCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Release Plan")
                .font(.headline)
            labeledValue("Component", viewModel.selectedComponent?.id ?? "-")
            labeledValue("Baseline", viewModel.changes?.baselineRef ?? "-")
            labeledValue("Current Version", viewModel.version?.version ?? "Unavailable")
            labeledValue("Recommended Bump", viewModel.releasePlan?.result?.bumpType ?? "-")
            if let skipped = viewModel.releasePlan?.result?.skippedReason {
                labeledValue("Dry-run Result", "Skipped: \(skipped)")
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private var commitsCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Changes Since Baseline")
                .font(.headline)
            if let commits = viewModel.changes?.commits, !commits.isEmpty {
                ForEach(commits.prefix(12)) { commit in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(commit.subject)
                        Text("\(commit.hash) · \(commit.category ?? "Other")")
                            .font(.caption.monospaced())
                            .foregroundColor(.secondary)
                    }
                }
                if (viewModel.changes?.commits.count ?? 0) > 12 {
                    Text("Showing first 12 commits. Full output is in the console.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            } else {
                Text("No changes loaded yet.")
                    .foregroundColor(.secondary)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private var releaseGuardCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Release Execution")
                .font(.headline)
            Text("This view only plans releases with `homeboy release --dry-run`. Mutating release execution stays disabled until the app has an explicit confirmation flow.")
                .foregroundColor(.secondary)
            Button("Release Disabled") { }
                .disabled(true)
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor))
        .cornerRadius(8)
    }

    private var consoleSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("CLI Output")
                    .font(.headline)
                Spacer()
                CopyButton.console(viewModel.consoleOutput, source: "Release")
                    .disabled(viewModel.consoleOutput.isEmpty)
                Button {
                    viewModel.consoleOutput = ""
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.consoleOutput.isEmpty)
            }
            ScrollView {
                Text(viewModel.consoleOutput.isEmpty ? "Ready to plan a release..." : viewModel.consoleOutput)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(viewModel.consoleOutput.isEmpty ? .secondary : .primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
            .background(Color(nsColor: .textBackgroundColor))
            .cornerRadius(8)
        }
        .padding()
    }

    private func labeledValue(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
                .foregroundColor(.secondary)
            Spacer()
            Text(value)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
