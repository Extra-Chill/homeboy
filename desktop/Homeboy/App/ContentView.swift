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
    case runHistory = "Run History"
    case release = "Release"
    case rigs = "Rigs"
    case stackManager = "Stacks"
    case git = "Git"
    case quality = "Quality"
    case remoteFileEditor = "File Editor"
    case remoteLogViewer = "Log Viewer"
    case databaseBrowser = "Database"
    case apiAuth = "API/Auth"
    case settings = "Settings"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .deployer: return "arrow.up.to.line"
        case .bench: return "speedometer"
        case .runHistory: return "clock.arrow.circlepath"
        case .release: return "tag"
        case .rigs: return "shippingbox.and.arrow.backward"
        case .stackManager: return "square.stack.3d.up"
        case .git: return "point.3.connected.trianglepath.dotted"
        case .quality: return "checkmark.seal"
        case .remoteFileEditor: return "doc.badge.gearshape"
        case .remoteLogViewer: return "doc.text.magnifyingglass"
        case .databaseBrowser: return "cylinder.split.1x2"
        case .apiAuth: return "network.badge.shield.half.filled"
        case .settings: return "gear"
        }
    }

    func isAvailable(for project: ProjectConfiguration?) -> Bool {
        guard let project else { return self == .settings }

        let hasComponents = !project.componentIds.isEmpty || !project.components.isEmpty
        let hasRemoteTarget = project.serverId?.isEmpty == false && project.basePath?.isEmpty == false

        switch self {
        case .settings:
            return true
        case .deployer, .bench, .runHistory, .release, .rigs, .stackManager, .git, .quality:
            return hasComponents
        case .remoteFileEditor, .remoteLogViewer:
            return hasRemoteTarget
        case .databaseBrowser:
            return project.isWordPress || !project.database.name.isEmpty
        case .apiAuth:
            return project.api.enabled || project.isWordPress
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
                ContentUnavailableView(
                    configManager.needsProjectCreation ? "No Homeboy Projects" : "Loading Homeboy Project",
                    systemImage: configManager.needsProjectCreation ? "folder.badge.plus" : "hourglass",
                    description: Text(
                        configManager.needsProjectCreation
                            ? "Create a project to start using Homeboy Desktop."
                            : "Reading project configuration from the Homeboy CLI."
                    )
                )
            }
        }
        .sheet(isPresented: $configManager.needsProjectCreation) {
            CreateProjectSheet(isFirstProject: true)
        }
        .onChange(of: configManager.activeProject?.id) { _, _ in
            ensureSelectedItemIsAvailable()
        }
    }

    private func ensureSelectedItemIsAvailable() {
        guard case .coreTool(let tool) = selectedItem,
              !tool.isAvailable(for: configManager.activeProject) else {
            return
        }

        selectedItem = CoreTool.allCases.first { $0 != .settings && $0.isAvailable(for: configManager.activeProject) }
            .map(NavigationItem.coreTool)
            ?? .coreTool(.settings)
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
            RunHistoryView()
                .opacity(selectedItem == .coreTool(.runHistory) ? 1 : 0)
            ReleaseWorkflowView()
                .opacity(selectedItem == .coreTool(.release) ? 1 : 0)
            RigsView()
                .opacity(selectedItem == .coreTool(.rigs) ? 1 : 0)
            StackManagerView()
                .opacity(selectedItem == .coreTool(.stackManager) ? 1 : 0)
            GitOperationsView()
                .opacity(selectedItem == .coreTool(.git) ? 1 : 0)
            QualityView()
                .opacity(selectedItem == .coreTool(.quality) ? 1 : 0)
            DatabaseBrowserView()
                .opacity(selectedItem == .coreTool(.databaseBrowser) ? 1 : 0)
            RemoteLogViewerView()
                .opacity(selectedItem == .coreTool(.remoteLogViewer) ? 1 : 0)
            RemoteFileEditorView()
                .opacity(selectedItem == .coreTool(.remoteFileEditor) ? 1 : 0)
            APIAuthWorkspaceView()
                .opacity(selectedItem == .coreTool(.apiAuth) ? 1 : 0)
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

// MARK: - Stack Manager

struct StackManagerView: View {
    @State private var stacks: [StackListItem] = []
    @State private var selectedStack: StackListItem?
    @State private var isLoading = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationSplitView {
            List(selection: $selectedStack) {
                Section("Stacks") {
                    ForEach(stacks) { stack in
                        StackRow(stack: stack)
                            .tag(stack)
                    }
                }
            }
            .navigationTitle("Stacks")
            .overlay {
                if isLoading && stacks.isEmpty {
                    ProgressView("Loading stacks...")
                } else if stacks.isEmpty && errorMessage == nil {
                    ContentUnavailableView(
                        "No Stack Specs",
                        systemImage: "square.stack.3d.up.slash",
                        description: Text("Create stack specs with homeboy stack create, then refresh this view.")
                    )
                }
            }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        Task { await loadStacks() }
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                    .disabled(isLoading)
                }
            }
        } detail: {
            if let selectedStack {
                StackDetailView(stack: selectedStack)
                    .id(selectedStack.id)
            } else {
                ContentUnavailableView(
                    "Select a Stack",
                    systemImage: "square.stack.3d.up",
                    description: Text("Choose a stack spec to inspect PR state and local branch status.")
                )
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundColor(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .background(Color.red.opacity(0.08))
            }
        }
        .task {
            await loadStacks()
        }
    }

    private func loadStacks() async {
        isLoading = true
        errorMessage = nil
        do {
            let loadedStacks = try await HomeboyCLI.shared.stackList()
            stacks = loadedStacks
            if selectedStack == nil || !loadedStacks.contains(where: { $0.id == selectedStack?.id }) {
                selectedStack = loadedStacks.first
            }
        } catch {
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }
}

struct StackRow: View {
    let stack: StackListItem

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Image(systemName: "square.stack.3d.up.fill")
                    .foregroundColor(.purple)
                Text(stack.id)
                    .font(.headline)
            }

            Text(stack.description)
                .font(.caption)
                .foregroundColor(.secondary)
                .lineLimit(2)

            HStack(spacing: 8) {
                Text(stack.component)
                Text("\(stack.prCount) PR\(stack.prCount == 1 ? "" : "s")")
            }
            .font(.caption2)
            .foregroundColor(.secondary)
        }
        .padding(.vertical, 4)
    }
}

struct StackDetailView: View {
    let stack: StackListItem

    @State private var spec: StackSpec?
    @State private var status: StackStatusOutput?
    @State private var inspection: StackInspectOutput?
    @State private var isLoading = false
    @State private var isInspecting = false
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()

            if isLoading && status == nil {
                ProgressView("Loading stack status...")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        if let errorMessage {
                            Text(errorMessage)
                                .foregroundColor(.red)
                                .padding(10)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Color.red.opacity(0.08))
                                .cornerRadius(8)
                        }

                        summaryCards
                        pullRequestList
                        inspectionSection
                    }
                    .padding()
                }
            }
        }
        .task {
            await loadStack()
        }
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 6) {
                Text(stack.id)
                    .font(.title)
                Text(stack.description)
                    .font(.subheadline)
                    .foregroundColor(.secondary)
                Text("\(stack.base) -> \(stack.target)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            Button {
                Task { await loadStack() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(isLoading)
        }
        .padding()
    }

    private var summaryCards: some View {
        HStack(spacing: 12) {
            StackMetricCard(title: "Pull Requests", value: "\(status?.prs.count ?? stack.prCount)")
            StackMetricCard(title: "Merged", value: "\(status?.mergedCount ?? 0)")
            StackMetricCard(title: "Target Ahead", value: status.map { "\($0.targetAhead)" } ?? "-")
            StackMetricCard(title: "Target Behind", value: status.map { "\($0.targetBehind)" } ?? "-")
        }
    }

    private var pullRequestList: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Pull Requests")
                .font(.headline)

            if let status {
                ForEach(status.prs) { pr in
                    StackPullRequestRow(pr: pr)
                }
            } else if let spec {
                ForEach(spec.prs) { pr in
                    HStack {
                        Text("#\(pr.number)")
                            .font(.headline)
                        VStack(alignment: .leading) {
                            Text(pr.repo)
                            if let note = pr.note {
                                Text(note)
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                        }
                        Spacer()
                    }
                    .padding(10)
                    .background(Color.secondary.opacity(0.08))
                    .cornerRadius(8)
                }
            }
        }
    }

    private var inspectionSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Branch Inspection")
                    .font(.headline)
                Spacer()
                Button {
                    Task { await inspectStackBranch() }
                } label: {
                    Label("Inspect", systemImage: "magnifyingglass")
                }
                .disabled(isInspecting || status == nil)
            }

            if isInspecting {
                ProgressView("Inspecting branch...")
            } else if let inspection {
                VStack(alignment: .leading, spacing: 4) {
                    Text("\(inspection.branch): \(inspection.commits.count) commit\(inspection.commits.count == 1 ? "" : "s") over \(inspection.base)")
                    Text("Merged PRs detected: \(inspection.mergedCount)")
                        .foregroundColor(.secondary)
                    ForEach(inspection.commits) { commit in
                        Text("\(commit.shortSha)  \(commit.subject)")
                            .font(.system(.caption, design: .monospaced))
                    }
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.secondary.opacity(0.08))
                .cornerRadius(8)
            } else {
                Text("Runs read-only `homeboy stack inspect` against the stack component path and base.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
    }

    private func loadStack() async {
        isLoading = true
        errorMessage = nil
        do {
            async let specTask = HomeboyCLI.shared.stackShow(id: stack.id)
            async let statusTask = HomeboyCLI.shared.stackStatus(id: stack.id)
            spec = try await specTask
            status = try await statusTask
        } catch {
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }

    private func inspectStackBranch() async {
        guard let status else { return }
        isInspecting = true
        errorMessage = nil
        do {
            inspection = try await HomeboyCLI.shared.stackInspect(path: status.componentPath, base: status.base, includePRs: false)
        } catch {
            errorMessage = error.localizedDescription
        }
        isInspecting = false
    }
}

struct StackMetricCard: View {
    let title: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.caption)
                .foregroundColor(.secondary)
            Text(value)
                .font(.title2)
                .fontWeight(.semibold)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(10)
    }
}

struct StackPullRequestRow: View {
    let pr: StackPullRequestStatus

    private var destination: URL? {
        URL(string: pr.url ?? "https://github.com/\(pr.repo)/pull/\(pr.number)")
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                if let destination {
                    Link("#\(pr.number) \(pr.title ?? pr.repo)", destination: destination)
                        .font(.headline)
                } else {
                    Text("#\(pr.number) \(pr.title ?? pr.repo)")
                        .font(.headline)
                }
                Text(pr.note ?? pr.repo)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            VStack(alignment: .trailing, spacing: 4) {
                StackStateBadge(label: pr.upstreamState, color: pr.upstreamState == "MERGED" ? .purple : .blue)
                StackStateBadge(label: pr.localState, color: pr.localState == "applied" ? .green : .orange)
                if let reviewDecision = pr.reviewDecision {
                    Text(reviewDecision)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(8)
    }
}

struct StackStateBadge: View {
    let label: String
    let color: Color

    var body: some View {
        Text(label)
            .font(.caption2)
            .fontWeight(.semibold)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(color.opacity(0.16))
            .foregroundColor(color)
            .cornerRadius(6)
    }
}

#Preview {
    ContentView()
        .environmentObject(AuthManager())
}
