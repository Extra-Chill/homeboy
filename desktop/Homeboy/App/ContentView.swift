import Combine
import SwiftUI

/// Navigation items: global orchestration domains and core tools are static,
/// extensions are dynamic. The app opens to the global shell (Activity)
/// without requiring an active project; only the `.coreTool` project tools
/// are gated on one.
enum NavigationItem: Hashable {
    case activity
    case missions
    case capacity
    case runners
    case coreTool(CoreTool)
    case extensionItem(String) // Extension ID
}

/// Built-in project tools (not extensions), reachable under the sidebar's
/// Projects section.
/// Settings is shown in a separate section.
enum CoreTool: String, CaseIterable, Identifiable {
    case commandBrowser = "Commands"
    case deployer = "Deployer"
    case bench = "Bench"
    case lab = "Lab"
    case runHistory = "Run History"
    case release = "Release"
    case rigs = "Rigs"
    case git = "Git"
    case quality = "Quality"
    case remoteFileEditor = "File Editor"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .commandBrowser: return "terminal"
        case .deployer: return "arrow.up.to.line"
        case .bench: return "speedometer"
        case .lab: return "desktopcomputer.and.arrow.down"
        case .runHistory: return "clock.arrow.circlepath"
        case .release: return "tag"
        case .rigs: return "shippingbox.and.arrow.backward"
        case .git: return "point.3.connected.trianglepath.dotted"
        case .quality: return "checkmark.seal"
        case .remoteFileEditor: return "doc.badge.gearshape"
        case .settings: return "gear"
        }
    }

    func isAvailable(for project: ProjectConfiguration?) -> Bool {
        guard let project else { return self == .settings }

        let hasComponents = !project.componentIds.isEmpty || !project.components.isEmpty
        let hasRemoteTarget = project.serverId?.isEmpty == false && project.basePath?.isEmpty == false

        switch self {
        case .commandBrowser:
            return true
        case .settings:
            return true
        case .deployer, .bench, .lab, .runHistory, .release, .rigs, .git, .quality:
            return hasComponents
        case .remoteFileEditor:
            return hasRemoteTarget
        }
    }
}

struct ContentView: View {
    @ObservedObject private var configManager = ConfigurationManager.shared
    @ObservedObject private var extensionManager = ExtensionManager.shared
    @StateObject private var missionStore = MissionStore()
    @StateObject private var navigationState = AppNavigationState()

    private var selectedItem: NavigationItem? { navigationState.selectedItem }

    var body: some View {
        NavigationSplitView {
            SidebarView(selectedItem: $navigationState.selectedItem)
        } detail: {
            detailView
        }
        .environmentObject(missionStore)
        .environmentObject(navigationState)
        .onChange(of: configManager.activeProject?.id) { _, _ in
            ensureSelectedItemIsAvailable()
        }
    }

    private func ensureSelectedItemIsAvailable() {
        guard case .coreTool(let tool) = selectedItem,
              tool != .settings,
              !tool.isAvailable(for: configManager.activeProject) else {
            return
        }

        // Fall back to the global Activity workspace rather than forcing a
        // project tool selection: orchestration domains never require a
        // project, so losing project-tool availability is not a dead end.
        navigationState.selectedItem = .activity
    }
    
    /// Views are kept mounted in a ZStack to preserve state (including running processes)
    /// when switching tabs. Only the selected view is visible via opacity.
    @ViewBuilder
    private var detailView: some View {
        ZStack {
            // Global orchestration workspace - no project required.
            ActivityView()
                .opacity(selectedItem == .activity ? 1 : 0)
            MissionsView()
                .opacity(selectedItem == .missions ? 1 : 0)
            CapacityView()
                .opacity(selectedItem == .capacity ? 1 : 0)
            RunnersView()
                .opacity(selectedItem == .runners ? 1 : 0)

            // Project tools - kept mounted to preserve state, gated on the
            // active project exactly as before.
            if configManager.activeProject != nil {
                CommandBrowserView()
                    .opacity(selectedItem == .coreTool(.commandBrowser) ? 1 : 0)
                DeployerView()
                    .opacity(selectedItem == .coreTool(.deployer) ? 1 : 0)
                BenchView()
                    .opacity(selectedItem == .coreTool(.bench) ? 1 : 0)
                HomeboyLabView()
                    .opacity(selectedItem == .coreTool(.lab) ? 1 : 0)
                RunHistoryView()
                    .opacity(selectedItem == .coreTool(.runHistory) ? 1 : 0)
                ReleaseWorkflowView()
                    .opacity(selectedItem == .coreTool(.release) ? 1 : 0)
                RigsView()
                    .opacity(selectedItem == .coreTool(.rigs) ? 1 : 0)
                GitOperationsView()
                    .opacity(selectedItem == .coreTool(.git) ? 1 : 0)
                QualityView()
                    .opacity(selectedItem == .coreTool(.quality) ? 1 : 0)
                RemoteFileEditorView()
                    .opacity(selectedItem == .coreTool(.remoteFileEditor) ? 1 : 0)

                // Dynamic extensions
                ForEach(extensionManager.extensions) { ext in
                    ExtensionContainerView(extensionId: ext.id)
                        .opacity(selectedItem == .extensionItem(ext.id) ? 1 : 0)
                }
            } else if isProjectScopedSelection {
                ProjectsEmptyStateView()
            }

            SettingsView()
                .opacity(selectedItem == .coreTool(.settings) ? 1 : 0)

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

    /// True when the current selection is a project tool (or extension) that
    /// needs an active project. Settings and the global orchestration
    /// domains are always reachable.
    private var isProjectScopedSelection: Bool {
        switch selectedItem {
        case .coreTool(let tool):
            return tool != .settings
        case .extensionItem:
            return true
        default:
            return false
        }
    }
}

/// Shown under Projects when no project is configured yet. Replaces the
/// previous whole-app blocker: the global orchestration domains (Activity,
/// Missions, Capacity, Runners) remain usable while this is on screen.
struct ProjectsEmptyStateView: View {
    @State private var showCreateProject = false

    var body: some View {
        ContentUnavailableView {
            Label("No Homeboy Projects", systemImage: "folder.badge.plus")
        } description: {
            Text("Create a project to use Deployer, Bench, Release, and the other project tools. Activity, Missions, Capacity, and Runners work without one.")
        } actions: {
            Button("Create Project") {
                showCreateProject = true
            }
            .buttonStyle(.borderedProminent)
        }
        .sheet(isPresented: $showCreateProject) {
            CreateProjectSheet(isFirstProject: false)
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
}
