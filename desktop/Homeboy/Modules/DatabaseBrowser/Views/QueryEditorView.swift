import SwiftUI

/// View for custom SQL query input and results display
struct QueryEditorView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    
    var body: some View {
        VStack(spacing: 0) {
            // Query input area
            queryInputSection
            
            Divider()
            
            // Results section
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
            // Results header
            resultsHeader
            
            Divider()
            
            // Results content
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
        Table(viewModel.customQueryRows, selection: $viewModel.selectedQueryRows) {
            
            TableColumnForEach(viewModel.customQueryColumns) { column in
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
                viewModel.copySelectedQueryRows()
            }
            .disabled(selectedIds.isEmpty)
        }
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
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 48))
                .foregroundColor(.red)
            
            Text("Query Error")
                .font(.headline)
                .foregroundColor(.red)
            
            Text(error)
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal)
                .textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    QueryEditorView(viewModel: DatabaseBrowserViewModel())
}
