import SwiftUI
import AppKit

/// View for custom SQL query input and results display
struct QueryEditorView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    
    var body: some View {
        VStack(spacing: 0) {
            queryInputSection
            
            Divider()
            
            resultsSection
        }
    }
    
    // MARK: - Query Input Section
    
    private var queryInputSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("SQL Query")
                .font(.headline)
            
            TextEditor(text: $viewModel.customQueryText)
                .font(.system(.body, design: .monospaced))
                .frame(minHeight: 100, maxHeight: 150)
                .border(Color(nsColor: .separatorColor), width: 1)
            
            HStack {
                Button(action: {
                    Task {
                        await viewModel.runCustomQuery()
                    }
                }) {
                    HStack(spacing: 4) {
                        Image(systemName: "play.fill")
                        Text("Run Query")
                    }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(viewModel.customQueryText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || viewModel.isRunningCustomQuery)
                
                Button(action: {
                    viewModel.clearCustomQuery()
                }) {
                    Text("Clear")
                }
                .disabled(viewModel.customQueryText.isEmpty && viewModel.customQueryRows.isEmpty)
                
                Spacer()
                
                if viewModel.isRunningCustomQuery {
                    ProgressView()
                        .scaleEffect(0.7)
                    Text("Running...")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
        }
        .padding()
    }
    
    // MARK: - Results Section
    
    private var resultsSection: some View {
        VStack(spacing: 0) {
            resultsHeader
            
            Divider()
            
            if let error = viewModel.customQueryError {
                errorView(error)
            } else if viewModel.isRunningCustomQuery {
                loadingView
            } else if !viewModel.hasExecutedQuery {
                emptyStateView
            } else if viewModel.customQueryColumns.isEmpty {
                noResultsView
            } else if viewModel.customQueryRows.isEmpty {
                noRowsView
            } else {
                resultsGrid
            }
        }
    }
    
    private var resultsHeader: some View {
        HStack {
            if viewModel.customQueryRowCount > 0 {
                Text("\(viewModel.customQueryRowCount) rows returned")
                    .font(.caption)
                    .foregroundColor(.secondary)
            } else if viewModel.customQueryColumns.isEmpty && viewModel.customQueryError == nil && !viewModel.isRunningCustomQuery {
                Text("Run a query to see results")
                    .font(.caption)
                    .foregroundColor(.secondary)
            } else if viewModel.customQueryRows.isEmpty && !viewModel.customQueryColumns.isEmpty {
                Text("Query executed successfully (no rows)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Results Grid
    
    private var resultsGrid: some View {
        NativeDataTable(
            items: viewModel.customQueryRows,
            columns: dynamicColumns,
            selection: $viewModel.selectedQueryRows,
            contextMenuProvider: { selectedIds in
                createContextMenu(for: selectedIds)
            }
        )
    }
    
    private var dynamicColumns: [DataTableColumn<TableRow>] {
        viewModel.customQueryColumns.map { column in
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
        
        let copyItem = NSMenuItem(title: "Copy", action: #selector(QueryMenuActions.copyRows), keyEquivalent: "c")
        copyItem.target = QueryMenuActions.shared
        copyItem.representedObject = QueryMenuActionContext(viewModel: viewModel, selectedIds: selectedIds)
        copyItem.isEnabled = !selectedIds.isEmpty
        menu.addItem(copyItem)
        
        return menu
    }
    
    // MARK: - State Views
    
    private var emptyStateView: some View {
        VStack(spacing: 12) {
            Image(systemName: "terminal")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("Enter a SQL query above and click Run")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var noRowsView: some View {
        VStack(spacing: 12) {
            Image(systemName: "checkmark.circle")
                .font(.system(size: 48))
                .foregroundColor(.green)
            
            Text("Query executed successfully")
                .foregroundColor(.secondary)
            
            Text("No rows returned")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var loadingView: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Running query...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var noResultsView: some View {
        VStack(spacing: 12) {
            Image(systemName: "checkmark.circle")
                .font(.system(size: 48))
                .foregroundColor(.green)
            
            Text("Query executed successfully")
                .font(.headline)
            
            Text("No results returned")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func errorView(_ error: String) -> some View {
        ErrorView(error, source: "Query Editor")
    }
}

// MARK: - Menu Action Helpers

struct QueryMenuActionContext {
    let viewModel: DatabaseBrowserViewModel
    let selectedIds: Set<Int>
}

@MainActor
class QueryMenuActions: NSObject {
    static let shared = QueryMenuActions()
    
    @objc func copyRows(_ sender: NSMenuItem) {
        guard let context = sender.representedObject as? QueryMenuActionContext else { return }
        context.viewModel.copySelectedQueryRows()
    }
}

#Preview {
    QueryEditorView(viewModel: DatabaseBrowserViewModel())
}
