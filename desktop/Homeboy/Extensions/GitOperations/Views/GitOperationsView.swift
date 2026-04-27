import AppKit
import SwiftUI

struct GitOperationsView: View {
    @StateObject private var viewModel = GitOperationsViewModel()
    @State private var sortDescriptor: DataTableSortDescriptor<GitComponentState>?

    var body: some View {
        VStack(spacing: 0) {
            headerSection
            Divider()

            if viewModel.components.isEmpty {
                emptyState
            } else {
                HSplitView {
                    componentListSection
                        .frame(minWidth: 420)

                    detailSection
                        .frame(minWidth: 360)
                }
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            viewModel.refresh()
        }
    }

    private var headerSection: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Git")
                    .font(.title2.bold())
                Text("Read-only component status and GitHub navigation")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if viewModel.isLoading {
                ProgressView()
                    .controlSize(.small)
                Text("Refreshing...")
                    .foregroundColor(.secondary)
            }

            Button {
                viewModel.refresh()
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .help("Refresh git status")
            .disabled(viewModel.isLoading)
        }
        .padding()
    }

    private var emptyState: some View {
        VStack(spacing: 20) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 56))
                .foregroundColor(.secondary)

            Text("No Components")
                .font(.title)

            Text("Add components in Settings to inspect their Git state.")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var componentListSection: some View {
        VStack(spacing: 0) {
            NativeDataTable(
                items: sortedComponents(viewModel.components),
                columns: makeColumns(),
                selection: Binding(
                    get: { viewModel.selectedComponentId.map { Set([$0]) } ?? [] },
                    set: { viewModel.selectedComponentId = $0.first }
                ),
                sortDescriptor: $sortDescriptor,
                onDoubleClick: { component in
                    viewModel.selectedComponentId = component.id
                }
            )

            Divider()

            HStack {
                Text("\(viewModel.components.count) components")
                    .foregroundColor(.secondary)
                    .font(.caption)
                Spacer()
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
    }

    private var detailSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let component = viewModel.selectedComponent {
                selectedComponentDetails(component)
            } else {
                ContentUnavailableView(
                    "Select a Component",
                    systemImage: "point.3.connected.trianglepath.dotted",
                    description: Text("Choose a component to inspect its status output and GitHub links.")
                )
            }
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func selectedComponentDetails(_ component: GitComponentState) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(component.displayName)
                    .font(.title2.bold())
                Text(component.path)
                    .font(.caption.monospaced())
                    .foregroundColor(.secondary)
                    .textSelection(.enabled)
            }

            HStack(spacing: 8) {
                Image(systemName: component.stateIcon)
                    .foregroundColor(Color(component.stateColor))
                Text(component.stateLabel)
                    .font(.headline)
            }

            if let error = component.error {
                InlineErrorView(error)
            }

            GroupBox("homeboy git status") {
                VStack(alignment: .leading, spacing: 8) {
                    if let status = component.status {
                        LabeledContent("Exit code", value: String(status.exitCode))
                        LabeledContent("Path", value: status.path)

                        if !status.stdout.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            Text(status.stdout)
                                .font(.system(.caption, design: .monospaced))
                                .textSelection(.enabled)
                        } else if status.success {
                            Text("No status output. The component is clean.")
                                .foregroundColor(.secondary)
                        }

                        if !status.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            Text(status.stderr)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundColor(.red)
                                .textSelection(.enabled)
                        }
                    } else {
                        Text("Status has not been loaded yet.")
                            .foregroundColor(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }

            GroupBox("GitHub") {
                HStack {
                    Button("Issues") {
                        viewModel.open(component.githubIssuesURL)
                    }
                    .disabled(component.githubIssuesURL == nil)

                    Button("Pull Requests") {
                        viewModel.open(component.githubPullRequestsURL)
                    }
                    .disabled(component.githubPullRequestsURL == nil)

                    Spacer()
                }
                .padding(.vertical, 4)
            }

            Spacer()
        }
    }

    private func sortedComponents(_ components: [GitComponentState]) -> [GitComponentState] {
        guard let descriptor = sortDescriptor else {
            return components.sorted { $0.displayName.localizedStandardCompare($1.displayName) == .orderedAscending }
        }
        return components.sorted { lhs, rhs in
            descriptor.compare(lhs, rhs) == .orderedAscending
        }
    }

    private func makeColumns() -> [DataTableColumn<GitComponentState>] {
        [
            .text(
                id: "component",
                title: "Component",
                width: .auto(min: 140, ideal: 180, max: 300),
                keyPath: \.displayName
            ),
            .custom(
                id: "state",
                title: "State",
                width: .fixed(120),
                sortable: true,
                sortComparator: { lhs, rhs in lhs.stateLabel.localizedStandardCompare(rhs.stateLabel) },
                cellProvider: { component in
                    makeIconTextCell(
                        text: component.stateLabel,
                        iconName: component.stateIcon,
                        iconColor: component.stateColor
                    )
                }
            ),
            .custom(
                id: "path",
                title: "Path",
                width: .auto(min: 180, ideal: 280, max: 500),
                sortComparator: { lhs, rhs in lhs.path.localizedStandardCompare(rhs.path) },
                cellProvider: { component in
                    makeTextCell(
                        text: component.path,
                        font: DataTableConstants.monospaceFont,
                        color: DataTableConstants.secondaryTextColor,
                        alignment: .left
                    )
                }
            )
        ]
    }
}

#Preview {
    GitOperationsView()
}
