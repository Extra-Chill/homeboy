import SwiftUI

/// Main content view showing table data in a grid format
struct TableDataView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    
    var body: some View {
        VStack(spacing: 0) {
            // Header with query toggle button
            contentHeader
            
            Divider()
            
            // Main content - either query editor or table data
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
                // Query mode header
                VStack(alignment: .leading, spacing: 2) {
                    Text("SQL Query")
                        .font(.headline)
                    
                    Text("Run custom queries against the database")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            } else if let table = viewModel.selectedTable {
                // Table info header
                VStack(alignment: .leading, spacing: 2) {
                    Text(table.name)
                        .font(.headline)
                    
                    Text("\(table.rowCount) total rows • \(table.engine) • \(table.formattedSize)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            } else {
                // No selection header
                Text("Database Browser")
                    .font(.headline)
            }
            
            Spacer()
            
            // Action buttons
            HStack(spacing: 8) {
                // Query mode toggle button
                Button(action: {
                    viewModel.toggleQueryMode()
                }) {
                    Image(systemName: viewModel.isQueryModeActive ? "tablecells" : "terminal")
                }
                .help(viewModel.isQueryModeActive ? "View Tables" : "SQL Query")
                
                // Refresh button (only in table mode)
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
                // Data grid
                if viewModel.isLoadingTableData {
                    loadingView
                } else if viewModel.rows.isEmpty && viewModel.totalRows == 0 {
                    emptyTableView
                } else {
                    dataGrid
                }
                
                Divider()
                
                // Pagination footer
                paginationFooter
            } else {
                emptyStateView
            }
        }
    }
    
    // MARK: - Data Grid
    
    private var dataGrid: some View {
        Table(viewModel.rows, selection: $viewModel.selectedRows) {
            TableColumnForEach(viewModel.columns) { column in
                TableColumn(column.name) { row in
                    Text(row.value(for: column.name).isEmpty ? "NULL" : row.value(for: column.name))
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(row.value(for: column.name).isEmpty ? .secondary : .primary)
                }
                .width(min: 80, ideal: 150)
            }
        }
        .contextMenu(forSelectionType: Int.self) { selectedIds in
            Button("Copy") {
                viewModel.copySelectedRows()
            }
            .disabled(selectedIds.isEmpty)
            
            if viewModel.columns.contains(where: { $0.isPrimaryKey }) {
                Divider()
                
                Button("Delete Row", role: .destructive) {
                    if let firstId = selectedIds.first,
                       let row = viewModel.rows.first(where: { $0.id == firstId }) {
                        viewModel.requestRowDeletion(row: row)
                    }
                }
                .disabled(selectedIds.count != 1)
            }
        }
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

#Preview {
    TableDataView(viewModel: DatabaseBrowserViewModel())
}
