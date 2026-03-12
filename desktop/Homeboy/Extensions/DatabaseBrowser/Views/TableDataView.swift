import SwiftUI
import AppKit

/// Main content view showing table data in a grid format
struct TableDataView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    
    var body: some View {
        VStack(spacing: 0) {
            contentHeader
            
            Divider()
            
            if viewModel.isQueryModeActive {
                QueryEditorView(viewModel: viewModel)
            } else {
                tableDataContent
            }
        }
    }
    
    // MARK: - Content Header
    
    private var contentHeader: some View {
        HStack {
            if viewModel.isQueryModeActive {
                VStack(alignment: .leading, spacing: 2) {
                    Text("SQL Query")
                        .font(.headline)
                    
                    Text("Run custom queries against the database")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            } else if let table = viewModel.selectedTable {
                VStack(alignment: .leading, spacing: 2) {
                    Text(table.name)
                        .font(.headline)
                    
                    Text("\(table.rowCount) total rows • \(table.engine) • \(table.formattedSize)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            } else {
                Text("Database Browser")
                    .font(.headline)
            }
            
            Spacer()
            
            HStack(spacing: 8) {
                Button(action: {
                    viewModel.toggleQueryMode()
                }) {
                    Image(systemName: viewModel.isQueryModeActive ? "tablecells" : "terminal")
                }
                .help(viewModel.isQueryModeActive ? "View Tables" : "SQL Query")
                
                if !viewModel.isQueryModeActive {
                    Button(action: {
                        Task {
                            await viewModel.refresh()
                        }
                    }) {
                        Image(systemName: "arrow.clockwise")
                    }
                    .help("Refresh")
                    .disabled(viewModel.selectedTable == nil)
                }
            }
        }
        .padding()
    }
    
    // MARK: - Table Data Content
    
    private var tableDataContent: some View {
        VStack(spacing: 0) {
            if let _ = viewModel.selectedTable {
                if viewModel.isLoadingTableData {
                    loadingView
                } else if viewModel.rows.isEmpty && viewModel.totalRows == 0 {
                    emptyTableView
                } else {
                    dataGrid
                }
                
                Divider()
                
                paginationFooter
            } else {
                emptyStateView
            }
        }
    }
    
    // MARK: - Data Grid
    
    private var dataGrid: some View {
        NativeDataTable(
            items: viewModel.rows,
            columns: dynamicColumns,
            selection: $viewModel.selectedRows,
            contextMenuProvider: { selectedIds in
                createContextMenu(for: selectedIds)
            }
        )
    }
    
    private var dynamicColumns: [DataTableColumn<TableRow>] {
        viewModel.columns.map { column in
            DataTableColumn<TableRow>.custom(
                id: column.name,
                title: column.name,
                width: .auto(min: 80, ideal: 150, max: 400),
                alignment: .left,
                sortable: true,
                sortComparator: { lhs, rhs in
                    let lhsValue = lhs.value(for: column.name)
                    let rhsValue = rhs.value(for: column.name)
                    return lhsValue.localizedStandardCompare(rhsValue)
                },
                cellProvider: { row in
                    let value = row.value(for: column.name)
                    return makeTextCell(
                        text: value.isEmpty ? "NULL" : value,
                        font: DataTableConstants.monospaceFont,
                        color: value.isEmpty ? DataTableConstants.nullTextColor : DataTableConstants.primaryTextColor,
                        alignment: .left
                    )
                }
            )
        }
    }
    
    private func createContextMenu(for selectedIds: Set<Int>) -> NSMenu {
        let menu = NSMenu()
        
        let copyItem = NSMenuItem(title: "Copy", action: #selector(DatabaseTableMenuActions.copyRows), keyEquivalent: "c")
        copyItem.target = DatabaseTableMenuActions.shared
        copyItem.representedObject = MenuActionContext(viewModel: viewModel, selectedIds: selectedIds)
        copyItem.isEnabled = !selectedIds.isEmpty
        menu.addItem(copyItem)
        
        if viewModel.columns.contains(where: { $0.isPrimaryKey }) {
            menu.addItem(NSMenuItem.separator())
            
            let deleteItem = NSMenuItem(title: "Delete Row", action: #selector(DatabaseTableMenuActions.deleteRow), keyEquivalent: "")
            deleteItem.target = DatabaseTableMenuActions.shared
            deleteItem.representedObject = MenuActionContext(viewModel: viewModel, selectedIds: selectedIds)
            deleteItem.isEnabled = selectedIds.count == 1
            menu.addItem(deleteItem)
        }
        
        return menu
    }
    
    // MARK: - Pagination Footer
    
    private var paginationFooter: some View {
        HStack {
            Text(viewModel.pageInfo)
                .font(.caption)
                .foregroundColor(.secondary)
            
            Spacer()
            
            HStack(spacing: 8) {
                Button(action: {
                    Task { await viewModel.previousPage() }
                }) {
                    Image(systemName: "chevron.left")
                }
                .disabled(!viewModel.canGoBack)
                
                Text("Page \(viewModel.currentPage) of \(viewModel.totalPages)")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .frame(minWidth: 100)
                
                Button(action: {
                    Task { await viewModel.nextPage() }
                }) {
                    Image(systemName: "chevron.right")
                }
                .disabled(!viewModel.canGoForward)
            }
        }
        .padding()
    }
    
    // MARK: - Empty States
    
    private var emptyStateView: some View {
        VStack(spacing: 12) {
            Image(systemName: "tablecells")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("Select a table to view data")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var emptyTableView: some View {
        VStack(spacing: 12) {
            Image(systemName: "tray")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("This table is empty")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var loadingView: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: - Menu Action Helpers

struct MenuActionContext {
    let viewModel: DatabaseBrowserViewModel
    let selectedIds: Set<Int>
}

@MainActor
class DatabaseTableMenuActions: NSObject {
    static let shared = DatabaseTableMenuActions()
    
    @objc func copyRows(_ sender: NSMenuItem) {
        guard let context = sender.representedObject as? MenuActionContext else { return }
        context.viewModel.copySelectedRows()
    }
    
    @objc func deleteRow(_ sender: NSMenuItem) {
        guard let context = sender.representedObject as? MenuActionContext,
              let firstId = context.selectedIds.first,
              let row = context.viewModel.rows.first(where: { $0.id == firstId }) else { return }
        context.viewModel.requestRowDeletion(row: row)
    }
}

#Preview {
    TableDataView(viewModel: DatabaseBrowserViewModel())
}
