import SwiftUI
import AppKit

/// Displays module results in a dynamic table based on output schema
struct ModuleResultsView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    @State private var sortDescriptor: DataTableSortDescriptor<IndexedRow>?
    
    private var columnNames: [String] {
        module.manifest.output?.schema.items?.keys.sorted() ?? []
    }
    
    var body: some View {
        VStack(spacing: 0) {
            header
            
            Divider()
            
            if viewModel.results.isEmpty {
                ContentUnavailableView(
                    "No Results",
                    systemImage: "doc.text",
                    description: Text("Run the module to see results")
                )
            } else {
                resultsTable
            }
        }
    }
    
    // MARK: - Header
    
    private var header: some View {
        HStack {
            Text("Results (\(viewModel.results.count))")
                .font(.headline)
            
            Spacer()
            
            if module.manifest.output?.selectable == true {
                Text("\(viewModel.selectedRows.count) selected")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Button("Select All") {
                    viewModel.selectAll()
                }
                .buttonStyle(.plain)
                .font(.caption)
                
                Button("Deselect All") {
                    viewModel.deselectAll()
                }
                .buttonStyle(.plain)
                .font(.caption)
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Results Table
    
    private var resultsTable: some View {
        NativeDataTable(
            items: sortedRows,
            columns: dynamicColumns,
            selection: $viewModel.selectedRows,
            sortDescriptor: $sortDescriptor
        )
    }
    
    private var sortedRows: [IndexedRow] {
        let rows = viewModel.results.enumerated().map { IndexedRow(index: $0.offset, data: $0.element) }
        guard let descriptor = sortDescriptor else {
            return rows
        }
        return rows.sorted { lhs, rhs in
            descriptor.compare(lhs, rhs) == .orderedAscending
        }
    }
    
    private var dynamicColumns: [DataTableColumn<IndexedRow>] {
        columnNames.map { columnName in
            DataTableColumn<IndexedRow>.custom(
                id: columnName,
                title: columnName.capitalized,
                width: .auto(min: 80, ideal: 150, max: 400),
                alignment: .left,
                sortable: true,
                sortComparator: { lhs, rhs in
                    let lhsValue = lhs.data[columnName]?.stringValue ?? ""
                    let rhsValue = rhs.data[columnName]?.stringValue ?? ""
                    return lhsValue.localizedStandardCompare(rhsValue)
                },
                cellProvider: { row in
                    let value = row.data[columnName]?.stringValue ?? ""
                    return makeTextCell(
                        text: value.isEmpty ? "—" : value,
                        font: DataTableConstants.defaultFont,
                        color: value.isEmpty ? DataTableConstants.nullTextColor : DataTableConstants.primaryTextColor,
                        alignment: .left
                    )
                }
            )
        }
    }
}

/// Wrapper to give each row an index for selection
struct IndexedRow: Identifiable {
    let index: Int
    let data: [String: AnyCodableValue]
    
    var id: Int { index }
}
