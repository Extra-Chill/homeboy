import SwiftUI
import AppKit
import SwiftUI

struct DeployerView: View {
    @StateObject private var viewModel = DeployerViewModel()
    @State private var sortDescriptor: DataTableSortDescriptor<DeployableComponent>?
    @State private var showingGroupEditor = false
    @State private var groupEditorMode: GroupingEditorSheet.Mode = .create

    
    var body: some View {
        VStack(spacing: 0) {
            if !viewModel.hasCredentials || !viewModel.hasSSHKey || !viewModel.hasBasePath {
                configurationRequiredView
            } else {
                headerSection
                Divider()
                HSplitView {
                    componentListSection
                        .frame(minWidth: 400)
                    consoleSection
                        .frame(minWidth: 300)
                }
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            viewModel.checkConfiguration()
            if viewModel.hasCredentials && viewModel.hasSSHKey && viewModel.hasBasePath {
                viewModel.refreshVersions()
            }
        }
        .alert("Deploy All Components?", isPresented: $viewModel.showDeployAllConfirmation) {
            Button("Cancel", role: .cancel) { }
            Button("Deploy All", role: .destructive) {
                viewModel.deployAll()
            }
        } message: {
            Text("This will deploy all \(viewModel.components.count) components to production. This may take several minutes.")
        }
        .alert("Build Required", isPresented: $viewModel.showBuildConfirmation) {
            Button("Cancel", role: .cancel) {
                viewModel.cancelBuild()
            }
            Button("Build & Deploy") {
                viewModel.confirmBuildAndDeploy()
            }
        } message: {
            let names = viewModel.componentsNeedingBuild.map { $0.name }.joined(separator: ", ")
            Text("The following components have source/artifact version mismatches and need to be built:\n\n\(names)")
        }
        .sheet(isPresented: $showingGroupEditor) {
            GroupingEditorSheet(mode: groupEditorMode) { name in
                switch groupEditorMode {
                case .create:
                    viewModel.createGrouping(name: name, fromComponentIds: Array(viewModel.selectedComponents))

                case .rename(let grouping):
                    viewModel.renameGrouping(groupingId: grouping.id, newName: name)
                }
            }
        }
    }
    
    // MARK: - Configuration Required View
    
    private var configurationRequiredView: some View {
        VStack(spacing: 20) {
            Image(systemName: "server.rack")
                .font(.system(size: 60))
                .foregroundColor(.secondary)
            
            Text("Deployment")
                .font(.title)
            
            Text("Configure your server credentials and base path in Settings to enable deployment.")
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
            
            VStack(alignment: .leading, spacing: 8) {
                if let serverName = viewModel.serverName {
                    Text("Server: \(serverName)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                } else {
                    InlineWarningView(
                        "No server linked to project",
                        source: "Deployer"
                    )
                }
                
                Divider()
                
                HStack {
                    Image(systemName: viewModel.hasCredentials ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasCredentials ? .green : .secondary)
                    Text("Server credentials configured")
                }
                HStack {
                    Image(systemName: viewModel.hasSSHKey ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasSSHKey ? .green : .secondary)
                    Text("SSH key configured")
                }
                HStack {
                    Image(systemName: viewModel.hasBasePath ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasBasePath ? .green : .secondary)
                    Text("Remote base path configured")
                }
            }
            .padding()
            .background(Color(nsColor: .controlBackgroundColor))
            .cornerRadius(8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    // MARK: - Header Section
    
    private var headerSection: some View {
        HStack(spacing: 16) {
            Button("Deploy Selected (\(viewModel.selectedComponents.count))") {
                viewModel.deploySelected()
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.selectedComponents.isEmpty || viewModel.isDeploying || viewModel.isBuilding)

            Button("Deploy All") {
                viewModel.confirmDeployAll()
            }
            .buttonStyle(.bordered)
            .disabled(viewModel.isDeploying || viewModel.isBuilding)
            
            Spacer()
            
            if viewModel.isLoading {
                ProgressView()
                    .controlSize(.small)
                Text("Loading...")
                    .foregroundColor(.secondary)
            } else if viewModel.isBuilding {
                ProgressView()
                    .controlSize(.small)
                Text("Building...")
                    .foregroundColor(.secondary)
                Button("Cancel") {
                    viewModel.cancelBuild()
                }
                .buttonStyle(.bordered)
            } else if viewModel.isDeploying {
                ProgressView()
                    .controlSize(.small)
                Text("Deploying...")
                    .foregroundColor(.secondary)
                Button("Cancel") {
                    viewModel.cancelDeployment()
                }
                .buttonStyle(.bordered)
            } else {
                Button {
                    viewModel.refreshVersions()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Refresh versions")
            }
        }
        .padding()
    }
    
    // MARK: - Component List Section
    
    private var componentListSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    // Grouped components
                    ForEach(viewModel.groupedComponents, id: \.grouping.id) { group in
                        ComponentGroupSection(
                            grouping: group.grouping,
                            components: group.components,
                            isExpanded: group.isExpanded,
                            canMoveUp: viewModel.canMoveGroupingUp(groupingId: group.grouping.id),
                            canMoveDown: viewModel.canMoveGroupingDown(groupingId: group.grouping.id),
                            allGroupings: viewModel.availableGroupings,
                            selectedComponents: $viewModel.selectedComponents,
                            sortDescriptor: $sortDescriptor,
                            viewModel: viewModel,
                            onToggle: { viewModel.toggleGroupExpansion(groupingId: group.grouping.id) },
                            onRenameGroup: {
                                groupEditorMode = .rename(group.grouping)
                                showingGroupEditor = true
                            },
                            onMoveGroupUp: { viewModel.moveGroupingUp(groupingId: group.grouping.id) },
                            onMoveGroupDown: { viewModel.moveGroupingDown(groupingId: group.grouping.id) },
                            onDeleteGroup: { viewModel.deleteGrouping(groupingId: group.grouping.id) },
                            onCreateGroupFromSelection: { selectedIds in
                                viewModel.selectedComponents = selectedIds
                                groupEditorMode = .create
                                showingGroupEditor = true
                            },
                            onAddSelectionToGroup: { selectedIds, grouping in
                                viewModel.addComponentsToGrouping(componentIds: Array(selectedIds), groupingId: grouping.id)
                            },
                            onRemoveSelectionFromGroup: { selectedIds in
                                viewModel.removeComponentsFromGrouping(componentIds: Array(selectedIds), groupingId: group.grouping.id)
                            }
                        )
                    }
                    
                    // Ungrouped components ("Components" section)
                    if !viewModel.ungroupedComponents.isEmpty {
                        UngroupedComponentSection(
                            components: viewModel.ungroupedComponents,
                            isExpanded: viewModel.isUngroupedExpanded,
                            allGroupings: viewModel.availableGroupings,
                            selectedComponents: $viewModel.selectedComponents,
                            sortDescriptor: $sortDescriptor,
                            viewModel: viewModel,
                            onToggle: { viewModel.toggleUngroupedExpansion() },
                            onCreateGroupFromSelection: { selectedIds in
                                viewModel.selectedComponents = selectedIds
                                groupEditorMode = .create
                                showingGroupEditor = true
                            },
                            onAddSelectionToGroup: { selectedIds, grouping in
                                viewModel.addComponentsToGrouping(componentIds: Array(selectedIds), groupingId: grouping.id)
                            }
                        )
                    }
                }
                .padding(.vertical, 8)
            }
            
            Divider()
            
            // Selection controls
            HStack {
                Button("Select Deployable") {
                    viewModel.selectDeployable()
                }
                .buttonStyle(.borderless)
                .help("Select components with build artifacts that need updating")
                
                Button("Select All") {
                    viewModel.selectAll()
                }
                .buttonStyle(.borderless)
                
                Button("Deselect All") {
                    viewModel.deselectAll()
                }
                .buttonStyle(.borderless)
                
                Spacer()
                
                Text("\(viewModel.selectedComponents.count) selected")
                    .foregroundColor(.secondary)
                    .font(.caption)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
    }
    
    // MARK: - Console Section
    
    private var consoleSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Console Output")
                    .font(.headline)
                Spacer()
                CopyButton.console(viewModel.consoleOutput, source: "Deployer")
                    .disabled(viewModel.consoleOutput.isEmpty)
                Button {
                    viewModel.consoleOutput = ""
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.isDeploying)
                .help("Clear output")
            }
            
            ScrollViewReader { proxy in
                ScrollView {
                    Text(viewModel.consoleOutput.isEmpty ? "Ready to deploy..." : viewModel.consoleOutput)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(viewModel.consoleOutput.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .id("console-bottom")
                }
                .onChange(of: viewModel.consoleOutput) { _, _ in
                    proxy.scrollTo("console-bottom", anchor: .bottom)
                }
            }
            .background(Color(nsColor: .textBackgroundColor))
            .cornerRadius(8)
            
            if let error = viewModel.error {
                InlineErrorView(error)
            }
        }
        .padding()
    }
}

// MARK: - Component Group Section

struct ComponentGroupSection: View {

    let grouping: ItemGrouping
    let components: [DeployableComponent]
    let isExpanded: Bool
    let canMoveUp: Bool
    let canMoveDown: Bool
    let allGroupings: [ItemGrouping]
    @Binding var selectedComponents: Set<String>
    @Binding var sortDescriptor: DataTableSortDescriptor<DeployableComponent>?
    let viewModel: DeployerViewModel
    let onToggle: () -> Void
    let onRenameGroup: () -> Void
    let onMoveGroupUp: () -> Void
    let onMoveGroupDown: () -> Void
    let onDeleteGroup: () -> Void
    let onCreateGroupFromSelection: (Set<String>) -> Void
    let onAddSelectionToGroup: (Set<String>, ItemGrouping) -> Void
    let onRemoveSelectionFromGroup: (Set<String>) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            // Group header with context menu
            Button(action: onToggle) {
                HStack {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundColor(.secondary)
                        .frame(width: 12)
                    
                    Text(grouping.name.uppercased())
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(.secondary)
                    
                    Text("(\(components.count))")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary.opacity(0.7))
                    
                    Spacer()
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .contextMenu {
                GroupingContextMenuItems(
                    grouping: grouping,
                    canMoveUp: canMoveUp,
                    canMoveDown: canMoveDown,
                    onRename: onRenameGroup,
                    onMoveUp: onMoveGroupUp,
                    onMoveDown: onMoveGroupDown,
                    onDelete: onDeleteGroup
                )
            }
            
            // Component table (collapsible)
            if isExpanded {
                componentTable
            }
        }
    }
    
    private var componentTable: some View {
        NativeDataTable(
            items: sortedComponents(components),
            columns: makeColumns(),
            selection: $selectedComponents,
            sortDescriptor: $sortDescriptor,
            contextMenuProvider: { selectedIds in
                makeDeployerComponentContextMenu(
                    selectedIds: selectedIds,
                    groupingContext: .grouped(grouping: grouping),
                    allGroupings: allGroupings,
                    onCreateGroupFromSelection: {
                        onCreateGroupFromSelection($0)
                    },
                    onAddSelectionToGroup: { ids, grouping in
                        onAddSelectionToGroup(ids, grouping)
                    },
                    onRemoveSelectionFromGroup: {
                        onRemoveSelectionFromGroup($0)
                    },
                    onDeploySelected: { ids in
                        selectedComponents = ids
                        viewModel.deploySelected()
                    },
                    onRefreshVersions: {
                        viewModel.refreshVersions()
                    }
                )
            }
        )
        .frame(height: CGFloat(components.count) * DataTableConstants.defaultRowHeight + DataTableConstants.headerHeight)
        .id(viewModel.versionDataHash)
    }

    private func sortedComponents(_ components: [DeployableComponent]) -> [DeployableComponent] {
        guard let descriptor = sortDescriptor else {
            return components.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        }
        return components.sorted { lhs, rhs in
            descriptor.compare(lhs, rhs) == .orderedAscending
        }
    }

    private func makeColumns() -> [DataTableColumn<DeployableComponent>] {
        [
            .text(
                id: "name",
                title: "Component",
                width: .auto(min: 140, ideal: 180, max: 300),
                keyPath: \.name
            ),
            .custom(
                id: "source",
                title: "Source",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.sourceVersionDisplay(for: component)
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "artifact",
                title: "Artifact",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.artifactVersionDisplay(for: component)
                    let sourceVersion = viewModel.sourceVersionDisplay(for: component)
                    // Highlight mismatch in orange
                    let isMismatch = sourceVersion != "—" && version != "—" && sourceVersion != version
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: isMismatch ? .systemOrange : DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "remote",
                title: "Remote",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.remoteVersionDisplay(for: component)
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "status",
                title: "Status",
                width: .auto(min: 90, ideal: 110, max: 140),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    makeStatusCellForDeployStatus(viewModel.status(for: component))
                }
            )
        ]
    }


}

// MARK: - Ungrouped Component Section

struct UngroupedComponentSection: View {
    
    let components: [DeployableComponent]
    let isExpanded: Bool
    let allGroupings: [ItemGrouping]
    @Binding var selectedComponents: Set<String>
    @Binding var sortDescriptor: DataTableSortDescriptor<DeployableComponent>?
    let viewModel: DeployerViewModel
    let onToggle: () -> Void
    let onCreateGroupFromSelection: (Set<String>) -> Void
    let onAddSelectionToGroup: (Set<String>, ItemGrouping) -> Void
    
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            // Section header
            Button(action: onToggle) {
                HStack {
                    Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundColor(.secondary)
                        .frame(width: 12)
                    
                    Text("COMPONENTS")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(.secondary)
                    
                    Text("(\(components.count))")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary.opacity(0.7))
                    
                    Spacer()
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            
            // Component table (collapsible)
            if isExpanded {
                componentTable
            }
        }
    }
    
    private var componentTable: some View {
        NativeDataTable(
            items: sortedComponents(components),
            columns: makeColumns(),
            selection: $selectedComponents,
            sortDescriptor: $sortDescriptor,
            contextMenuProvider: { selectedIds in
                makeDeployerComponentContextMenu(
                    selectedIds: selectedIds,
                    groupingContext: .ungrouped,
                    allGroupings: allGroupings,
                    onCreateGroupFromSelection: {
                        onCreateGroupFromSelection($0)
                    },
                    onAddSelectionToGroup: { ids, grouping in
                        onAddSelectionToGroup(ids, grouping)
                    },
                    onRemoveSelectionFromGroup: { _ in },
                    onDeploySelected: { ids in
                        selectedComponents = ids
                        viewModel.deploySelected()
                    },
                    onRefreshVersions: {
                        viewModel.refreshVersions()
                    }
                )
            }
        )
        .frame(height: CGFloat(components.count) * DataTableConstants.defaultRowHeight + DataTableConstants.headerHeight)
        .id(viewModel.versionDataHash)
    }

    private func sortedComponents(_ components: [DeployableComponent]) -> [DeployableComponent] {
        guard let descriptor = sortDescriptor else {
            return components.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        }
        return components.sorted { lhs, rhs in
            descriptor.compare(lhs, rhs) == .orderedAscending
        }
    }

    private func makeColumns() -> [DataTableColumn<DeployableComponent>] {
        [
            .text(
                id: "name",
                title: "Component",
                width: .auto(min: 140, ideal: 180, max: 300),
                keyPath: \.name
            ),
            .custom(
                id: "source",
                title: "Source",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.sourceVersionDisplay(for: component)
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "artifact",
                title: "Artifact",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.artifactVersionDisplay(for: component)
                    let sourceVersion = viewModel.sourceVersionDisplay(for: component)
                    // Highlight mismatch in orange
                    let isMismatch = sourceVersion != "—" && version != "—" && sourceVersion != version
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: isMismatch ? .systemOrange : DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "remote",
                title: "Remote",
                width: .fixed(60),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    let version = viewModel.remoteVersionDisplay(for: component)
                    return makeTextCell(
                        text: version,
                        font: DataTableConstants.monospaceFont,
                        color: DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            ),
            .custom(
                id: "status",
                title: "Status",
                width: .auto(min: 90, ideal: 110, max: 140),
                alignment: .left,
                sortable: false,
                cellProvider: { component in
                    makeStatusCellForDeployStatus(viewModel.status(for: component))
                }
            )
        ]
    }

}

// MARK: - Helper Functions

private func makeStatusCellForDeployStatus(_ status: DeployStatus) -> NSView {
    switch status {
    case .current:
        return makeStatusCell(text: "Current", iconName: "checkmark.circle.fill", color: .systemGreen)
    case .needsUpdate:
        return makeStatusCell(text: "Update", iconName: "arrow.up.circle.fill", color: .systemOrange)
    case .notDeployed:
        return makeStatusCell(text: "Not Deployed", iconName: "xmark.circle.fill", color: .systemRed)
    case .buildRequired:
        return makeStatusCell(text: "Build Required", iconName: "hammer.fill", color: .systemYellow)
    case .unknown:
        return makeStatusCell(text: "Unknown", iconName: "questionmark.circle", color: .systemGray)
    case .deploying:
        return makeLoadingCell(text: "...")
    case .failed:
        return makeStatusCell(text: "Failed", iconName: "exclamationmark.triangle.fill", color: .systemRed)
    }
}

#Preview {
    DeployerView()
}
