import AppKit
import SwiftUI

/// Sidebar view showing table groupings with expandable table lists
struct SiteListView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    @State private var showingGroupEditor = false
    @State private var groupEditorMode: GroupingEditorSheet.Mode = .create
    @State private var pendingNewGroupTableName: String? = nil
    
    var body: some View {
        List {
            // Grouped tables sections
            ForEach(Array(viewModel.groupedTables.enumerated()), id: \.element.grouping.id) { index, group in
                GroupSection(
                    grouping: group.grouping,
                    tables: group.tables,
                    isExpanded: group.isExpanded,
                    selectedTable: viewModel.selectedTable,
                    selectedTableNames: viewModel.selectedTableNames,
                    allGroupings: viewModel.groupedTables.map { $0.grouping },
                    canMoveUp: viewModel.canMoveGroupingUp(groupingId: group.grouping.id),
                    canMoveDown: viewModel.canMoveGroupingDown(groupingId: group.grouping.id),
                    onToggle: { viewModel.toggleGroupExpansion(groupingId: group.grouping.id) },
                    onSelectTable: { table in
                        Task {
                            await viewModel.selectTable(table)
                        }
                    },
                    onToggleMultiSelect: { table in
                        viewModel.toggleTableSelection(table.name)
                    },
                    isTableProtected: { viewModel.isTableProtected($0) },
                    isCoreProtected: { viewModel.isCoreProtectedTable($0.name) },
                    isUnlocked: { viewModel.isTableUnlocked($0.name) },
                    isInGroup: { viewModel.isTableInGroupByMembership($0.name) },
                    onDeleteTable: { viewModel.requestTableDeletion($0) },
                    onRenameGroup: {
                        groupEditorMode = .rename(group.grouping)
                        showingGroupEditor = true
                    },
                    onMoveGroupUp: { viewModel.moveGroupingUp(groupingId: group.grouping.id) },
                    onMoveGroupDown: { viewModel.moveGroupingDown(groupingId: group.grouping.id) },
                    onDeleteGroup: { viewModel.deleteGrouping(groupingId: group.grouping.id) },
                    onSelectAllInGroup: { viewModel.selectAllTablesInGroup(groupingId: group.grouping.id) },
                    onAddTableToGroup: { table, grouping in
                        viewModel.addTablesToGrouping(tableNames: [table.name], groupingId: grouping.id)
                    },
                    onRemoveTableFromGroup: { table in
                        if let currentGrouping = viewModel.groupingForTable(table.name) {
                            viewModel.removeTablesFromGrouping(tableNames: [table.name], groupingId: currentGrouping.id)
                        }
                    },
                    onCreateNewGroup: { tableName in
                        pendingNewGroupTableName = tableName
                        groupEditorMode = .create
                        showingGroupEditor = true
                    },
                    onProtectTable: { viewModel.protectTable($0.name) },
                    onUnprotectTable: { viewModel.unprotectTable($0.name) },
                    onUnlockTable: { viewModel.unlockTable($0.name) }
                )
            }

            // Ungrouped tables section
            if !viewModel.ungroupedTables.isEmpty {
                UngroupedSection(
                    tables: viewModel.ungroupedTables,
                    isExpanded: viewModel.isUngroupedExpanded,
                    selectedTable: viewModel.selectedTable,
                    selectedTableNames: viewModel.selectedTableNames,
                    allGroupings: viewModel.groupedTables.map { $0.grouping },
                    onToggle: { viewModel.toggleUngroupedExpansion() },
                    onSelectTable: { table in
                        Task {
                            await viewModel.selectTable(table)
                        }
                    },
                    onToggleMultiSelect: { table in
                        viewModel.toggleTableSelection(table.name)
                    },
                    isTableProtected: { viewModel.isTableProtected($0) },
                    isCoreProtected: { viewModel.isCoreProtectedTable($0.name) },
                    isUnlocked: { viewModel.isTableUnlocked($0.name) },
                    onDeleteTable: { viewModel.requestTableDeletion($0) },
                    onSelectAllUngrouped: { viewModel.selectAllUngroupedTables() },
                    onAddTableToGroup: { table, grouping in
                        viewModel.addTablesToGrouping(tableNames: [table.name], groupingId: grouping.id)
                    },
                    onCreateNewGroup: { tableName in
                        pendingNewGroupTableName = tableName
                        groupEditorMode = .create
                        showingGroupEditor = true
                    },
                    onProtectTable: { viewModel.protectTable($0.name) },
                    onUnprotectTable: { viewModel.unprotectTable($0.name) },
                    onUnlockTable: { viewModel.unlockTable($0.name) }
                )
            }
        }
        .listStyle(.sidebar)
        .contextMenu {
            Button("Regenerate Default Groupings...") {
                viewModel.requestRegenerateDefaultGroupings()
            }

            if !viewModel.selectedTableNames.isEmpty {
                Divider()

                Text("\(viewModel.selectedTableNames.count) tables selected")
                    .foregroundColor(.secondary)

                Menu("Add Selected to Group...") {
                    ForEach(viewModel.groupedTables.map { $0.grouping }) { grouping in
                        Button(grouping.name) {
                            viewModel.addSelectedTablesToGrouping(groupingId: grouping.id)
                        }
                    }
                }

                Button("Protect Selected Tables") {
                    viewModel.protectSelectedTables()
                }

                Button("Clear Selection") {
                    viewModel.clearTableSelection()
                }
            }
        }
        .alert("Regenerate Default Groupings?", isPresented: $viewModel.showRegenerateGroupingsConfirm) {
            Button("Cancel", role: .cancel) {
                viewModel.cancelRegenerateDefaultGroupings()
            }
            Button("Regenerate", role: .destructive) {
                viewModel.confirmRegenerateDefaultGroupings()
            }
        } message: {
            Text("This will replace all custom groupings with defaults based on your project configuration. This cannot be undone.")
        }
        .sheet(isPresented: $showingGroupEditor) {
            GroupingEditorSheet(mode: groupEditorMode) { name in
                switch groupEditorMode {
                case .create:
                    if let tableName = pendingNewGroupTableName {
                        viewModel.createGrouping(name: name, fromTableNames: [tableName])
                        pendingNewGroupTableName = nil
                    } else {
                        viewModel.createGrouping(name: name, fromTableNames: [])
                    }
                case .rename(let grouping):
                    viewModel.renameGrouping(groupingId: grouping.id, newName: name)
                }
            }
        }
    }
}

/// A collapsible section for a table grouping
struct GroupSection: View {
    let grouping: ItemGrouping
    let tables: [DatabaseTable]
    let isExpanded: Bool
    let selectedTable: DatabaseTable?
    let selectedTableNames: Set<String>
    let allGroupings: [ItemGrouping]
    let canMoveUp: Bool
    let canMoveDown: Bool
    let onToggle: () -> Void
    let onSelectTable: (DatabaseTable) -> Void
    let onToggleMultiSelect: (DatabaseTable) -> Void
    let isTableProtected: (DatabaseTable) -> Bool
    let isCoreProtected: (DatabaseTable) -> Bool
    let isUnlocked: (DatabaseTable) -> Bool
    let isInGroup: (DatabaseTable) -> Bool
    let onDeleteTable: (DatabaseTable) -> Void
    let onRenameGroup: () -> Void
    let onMoveGroupUp: () -> Void
    let onMoveGroupDown: () -> Void
    let onDeleteGroup: () -> Void
    let onSelectAllInGroup: () -> Void
    let onAddTableToGroup: (DatabaseTable, ItemGrouping) -> Void
    let onRemoveTableFromGroup: (DatabaseTable) -> Void
    let onCreateNewGroup: (String) -> Void
    let onProtectTable: (DatabaseTable) -> Void
    let onUnprotectTable: (DatabaseTable) -> Void
    let onUnlockTable: (DatabaseTable) -> Void

    var body: some View {
        DisclosureGroup(isExpanded: Binding(
            get: { isExpanded },
            set: { _ in onToggle() }
        )) {
            ForEach(tables) { table in
                TableRowView(
                    table: table,
                    isSelected: selectedTable?.id == table.id,
                    isMultiSelected: selectedTableNames.contains(table.name),
                    isProtected: isTableProtected(table),
                    isCoreProtected: isCoreProtected(table),
                    isUnlocked: isUnlocked(table),
                    isInGroup: isInGroup(table),
                    allGroupings: allGroupings,
                    onDelete: { onDeleteTable(table) },
                    onAddToGroup: { grouping in onAddTableToGroup(table, grouping) },
                    onRemoveFromGroup: { onRemoveTableFromGroup(table) },
                    onCreateNewGroup: { onCreateNewGroup(table.name) },
                    onProtect: { onProtectTable(table) },
                    onUnprotect: { onUnprotectTable(table) },
                    onUnlock: { onUnlockTable(table) },
                    onToggleMultiSelect: { onToggleMultiSelect(table) }
                )
                .contentShape(Rectangle())
                .onTapGesture {
                    if NSEvent.modifierFlags.contains(.command) {
                        onToggleMultiSelect(table)
                    } else {
                        onSelectTable(table)
                    }
                }
            }
        } label: {
            GroupHeaderView(grouping: grouping, tableCount: tables.count)
                .contextMenu {
                    Button("Select All Tables") {
                        onSelectAllInGroup()
                    }

                    Divider()

                    Button("Rename Group...") {
                        onRenameGroup()
                    }

                    Divider()

                    Button("Move Up") {
                        onMoveGroupUp()
                    }
                    .disabled(!canMoveUp)

                    Button("Move Down") {
                        onMoveGroupDown()
                    }
                    .disabled(!canMoveDown)

                    Divider()

                    Button("Delete Group", role: .destructive) {
                        onDeleteGroup()
                    }
                }
        }
    }
}

/// Header for a grouping section showing name and table count
struct GroupHeaderView: View {
    let grouping: ItemGrouping
    let tableCount: Int
    
    var body: some View {
        HStack {
            Text(grouping.name)
                .font(.headline)
                .lineLimit(1)
            
            Spacer()
            
            Text("\(tableCount)")
                .font(.caption)
                .foregroundColor(.secondary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Color.secondary.opacity(0.2))
                .cornerRadius(4)
        }
        .padding(.vertical, 4)
    }
}

/// Section for ungrouped tables (appears at bottom with minimal styling)
struct UngroupedSection: View {
    let tables: [DatabaseTable]
    let isExpanded: Bool
    let selectedTable: DatabaseTable?
    let selectedTableNames: Set<String>
    let allGroupings: [ItemGrouping]
    let onToggle: () -> Void
    let onSelectTable: (DatabaseTable) -> Void
    let onToggleMultiSelect: (DatabaseTable) -> Void
    let isTableProtected: (DatabaseTable) -> Bool
    let isCoreProtected: (DatabaseTable) -> Bool
    let isUnlocked: (DatabaseTable) -> Bool
    let onDeleteTable: (DatabaseTable) -> Void
    let onSelectAllUngrouped: () -> Void
    let onAddTableToGroup: (DatabaseTable, ItemGrouping) -> Void
    let onCreateNewGroup: (String) -> Void
    let onProtectTable: (DatabaseTable) -> Void
    let onUnprotectTable: (DatabaseTable) -> Void
    let onUnlockTable: (DatabaseTable) -> Void

    var body: some View {
        DisclosureGroup(isExpanded: Binding(
            get: { isExpanded },
            set: { _ in onToggle() }
        )) {
            ForEach(tables) { table in
                TableRowView(
                    table: table,
                    isSelected: selectedTable?.id == table.id,
                    isMultiSelected: selectedTableNames.contains(table.name),
                    isProtected: isTableProtected(table),
                    isCoreProtected: isCoreProtected(table),
                    isUnlocked: isUnlocked(table),
                    isInGroup: false,
                    allGroupings: allGroupings,
                    onDelete: { onDeleteTable(table) },
                    onAddToGroup: { grouping in onAddTableToGroup(table, grouping) },
                    onRemoveFromGroup: { },
                    onCreateNewGroup: { onCreateNewGroup(table.name) },
                    onProtect: { onProtectTable(table) },
                    onUnprotect: { onUnprotectTable(table) },
                    onUnlock: { onUnlockTable(table) },
                    onToggleMultiSelect: { onToggleMultiSelect(table) }
                )
                .contentShape(Rectangle())
                .onTapGesture {
                    if NSEvent.modifierFlags.contains(.command) {
                        onToggleMultiSelect(table)
                    } else {
                        onSelectTable(table)
                    }
                }
            }
        } label: {
            HStack {
                Text("\(tables.count) ungrouped")
                    .font(.subheadline)
                    .foregroundColor(.secondary)

                Spacer()
            }
            .padding(.vertical, 4)
            .contextMenu {
                Button("Select All Ungrouped") {
                    onSelectAllUngrouped()
                }
            }
        }
    }
}

/// Individual table row in the list
struct TableRowView: View {
    let table: DatabaseTable
    let isSelected: Bool
    let isMultiSelected: Bool
    let isProtected: Bool
    let isCoreProtected: Bool
    let isUnlocked: Bool
    let isInGroup: Bool
    let allGroupings: [ItemGrouping]
    let onDelete: () -> Void
    let onAddToGroup: (ItemGrouping) -> Void
    let onRemoveFromGroup: () -> Void
    let onCreateNewGroup: () -> Void
    let onProtect: () -> Void
    let onUnprotect: () -> Void
    let onUnlock: () -> Void
    let onToggleMultiSelect: () -> Void

    private var rowBackground: Color {
        if isSelected {
            return Color.accentColor.opacity(0.2)
        } else if isMultiSelected {
            return Color.accentColor.opacity(0.1)
        }
        return Color.clear
    }

    var body: some View {
        HStack {
            if isMultiSelected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.accentColor)
                    .font(.caption)
            }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(table.name)
                        .font(.body)
                        .lineLimit(1)

                    if isProtected && !isUnlocked {
                        Image(systemName: "lock.fill")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                            .help("Protected table - cannot be dropped")
                    } else if isUnlocked {
                        Image(systemName: "lock.open.fill")
                            .font(.caption2)
                            .foregroundColor(.orange)
                            .help("Unlocked - deletion allowed")
                    }
                }

                HStack(spacing: 8) {
                    Text("\(table.rowCount) rows")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    Text(table.formattedSize)
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }

            Spacer()
        }
        .padding(.vertical, 2)
        .padding(.leading, 8)
        .background(rowBackground)
        .cornerRadius(4)
        .contextMenu {
            // Copy
            Button {
                let tableInfo = "\(table.name)\t\(table.rowCount) rows\t\(table.formattedSize)"
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(tableInfo, forType: .string)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            
            Divider()
            
            // Group management
            Menu("Add to Group...") {
                ForEach(allGroupings) { grouping in
                    Button(grouping.name) {
                        onAddToGroup(grouping)
                    }
                }
                
                if !allGroupings.isEmpty {
                    Divider()
                }
                
                Button("New Group...") {
                    onCreateNewGroup()
                }
            }
            
            if isInGroup {
                Button("Remove from Group") {
                    onRemoveFromGroup()
                }
            }
            
            Divider()
            
            // Protection management
            if isProtected {
                if isCoreProtected {
                    if isUnlocked {
                        Button("Re-lock Table") {
                            onUnprotect()
                        }
                    } else {
                        Button("Unlock Table...") {
                            onUnlock()
                        }
                        .help("Allow deletion of this protected table")
                    }
                } else {
                    Button("Remove Protection") {
                        onUnprotect()
                    }
                }
            } else {
                Button("Protect Table") {
                    onProtect()
                }
                .help("Prevent accidental deletion")
            }
            
            Divider()
            
            // Delete
            Button(role: .destructive) {
                onDelete()
            } label: {
                Label("Drop Table...", systemImage: "trash")
            }
            .disabled(isProtected && !isUnlocked)
        }
    }
}

#Preview {
    SiteListView(viewModel: DatabaseBrowserViewModel())
        .frame(width: 280)
}
