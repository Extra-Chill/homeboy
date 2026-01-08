import SwiftUI

/// Displays module results in a dynamic table based on output schema
struct ModuleResultsView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    private var columns: [String] {
        module.manifest.output.schema.items?.keys.sorted() ?? []
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Results (\(viewModel.results.count))")
                    .font(.headline)
                
                Spacer()
                
                if module.manifest.output.selectable {
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
            
            Divider()
            
            // Table
            if viewModel.results.isEmpty {
                ContentUnavailableView(
                    "No Results",
                    systemImage: "doc.text",
                    description: Text("Run the module to see results")
                )
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        // Header row
                        HStack(spacing: 0) {
                            if module.manifest.output.selectable {
                                Text("")
                                    .frame(width: 30)
                            }
                            ForEach(columns, id: \.self) { column in
                                Text(column.capitalized)
                                    .font(.headline)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.horizontal, 8)
                            }
                        }
                        .padding(.vertical, 8)
                        .background(Color(NSColor.controlBackgroundColor))
                        
                        Divider()
                        
                        // Data rows
                        ForEach(indexedRows) { row in
                            HStack(spacing: 0) {
                                if module.manifest.output.selectable {
                                    Image(systemName: viewModel.selectedRows.contains(row.index) ? "checkmark.circle.fill" : "circle")
                                        .foregroundColor(viewModel.selectedRows.contains(row.index) ? .accentColor : .secondary)
                                        .frame(width: 30)
                                        .onTapGesture {
                                            viewModel.toggleRowSelection(row.index)
                                        }
                                }
                                ForEach(columns, id: \.self) { column in
                                    Text(row.data[column]?.stringValue ?? "")
                                        .lineLimit(2)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .padding(.horizontal, 8)
                                }
                            }
                            .padding(.vertical, 6)
                            .background(row.index % 2 == 0 ? Color.clear : Color(NSColor.controlBackgroundColor).opacity(0.5))
                            .contentShape(Rectangle())
                            .onTapGesture {
                                if module.manifest.output.selectable {
                                    viewModel.toggleRowSelection(row.index)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    private var indexedRows: [IndexedRow] {
        viewModel.results.enumerated().map { IndexedRow(index: $0.offset, data: $0.element) }
    }
}

/// Wrapper to give each row an index for selection
struct IndexedRow: Identifiable {
    let index: Int
    let data: [String: AnyCodableValue]
    
    var id: Int { index }
}
