import SwiftUI

/// Sidebar view showing WordPress sites with expandable table lists
struct SiteListView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel
    
    var body: some View {
        List {
            // Sites section
            ForEach(viewModel.sites) { site in
                SiteSection(
                    site: site,
                    selectedTable: viewModel.selectedTable,
                    onToggle: { viewModel.toggleSiteExpansion(site) },
                    onSelectTable: { table in
                        Task {
                            await viewModel.selectTable(table)
                        }
                    },
                    isTableProtected: { viewModel.isTableProtected($0) },
                    onDeleteTable: { viewModel.requestTableDeletion($0) }
                )
            }
            
            // Network tables section
            if viewModel.networkCategory.tableCount > 0 {
                CategorySection(
                    category: viewModel.networkCategory,
                    selectedTable: viewModel.selectedTable,
                    onToggle: { viewModel.toggleNetworkExpansion() },
                    onSelectTable: { table in
                        Task {
                            await viewModel.selectTable(table)
                        }
                    },
                    isTableProtected: { viewModel.isTableProtected($0) },
                    onDeleteTable: { viewModel.requestTableDeletion($0) }
                )
            }
            
            // Other tables section (only show if tables exist)
            if viewModel.otherCategory.tableCount > 0 {
                CategorySection(
                    category: viewModel.otherCategory,
                    selectedTable: viewModel.selectedTable,
                    onToggle: { viewModel.toggleOtherExpansion() },
                    onSelectTable: { table in
                        Task {
                            await viewModel.selectTable(table)
                        }
                    },
                    isTableProtected: { viewModel.isTableProtected($0) },
                    onDeleteTable: { viewModel.requestTableDeletion($0) }
                )
            }
        }
        .listStyle(.sidebar)
    }
}

/// A collapsible section for a WordPress site
struct SiteSection: View {
    let site: WordPressSite
    let selectedTable: DatabaseTable?
    let onToggle: () -> Void
    let onSelectTable: (DatabaseTable) -> Void
    let isTableProtected: (DatabaseTable) -> Bool
    let onDeleteTable: (DatabaseTable) -> Void
    
    var body: some View {
        DisclosureGroup(isExpanded: Binding(
            get: { site.isExpanded },
            set: { _ in onToggle() }
        )) {
            ForEach(site.tables) { table in
                TableRowView(
                    table: table,
                    isSelected: selectedTable?.id == table.id,
                    isProtected: isTableProtected(table),
                    onDelete: { onDeleteTable(table) }
                )
                .contentShape(Rectangle())
                .onTapGesture {
                    onSelectTable(table)
                }
            }
        } label: {
            SiteHeaderView(site: site)
        }
    }
}

/// Header for a site section showing name, domain, and table count
struct SiteHeaderView: View {
    let site: WordPressSite
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(site.name)
                    .font(.headline)
                    .lineLimit(1)
                
                Text(site.domain)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }
            
            Spacer()
            
            Text("\(site.tableCount)")
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

/// A collapsible section for a table category (Network, Other)
struct CategorySection: View {
    let category: TableCategory
    let selectedTable: DatabaseTable?
    let onToggle: () -> Void
    let onSelectTable: (DatabaseTable) -> Void
    let isTableProtected: (DatabaseTable) -> Bool
    let onDeleteTable: (DatabaseTable) -> Void
    
    var body: some View {
        DisclosureGroup(isExpanded: Binding(
            get: { category.isExpanded },
            set: { _ in onToggle() }
        )) {
            ForEach(category.tables) { table in
                TableRowView(
                    table: table,
                    isSelected: selectedTable?.id == table.id,
                    isProtected: isTableProtected(table),
                    onDelete: { onDeleteTable(table) }
                )
                .contentShape(Rectangle())
                .onTapGesture {
                    onSelectTable(table)
                }
            }
        } label: {
            CategoryHeaderView(category: category)
        }
    }
}

/// Header for a category section
struct CategoryHeaderView: View {
    let category: TableCategory
    
    var body: some View {
        HStack {
            Text(category.name)
                .font(.headline)
            
            Spacer()
            
            Text("\(category.tableCount)")
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

/// Individual table row in the list
struct TableRowView: View {
    let table: DatabaseTable
    let isSelected: Bool
    let isProtected: Bool
    let onDelete: () -> Void
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(table.name)
                        .font(.body)
                        .lineLimit(1)
                    
                    if isProtected {
                        Image(systemName: "lock.fill")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                            .help("Protected table - cannot be dropped")
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
        .background(isSelected ? Color.accentColor.opacity(0.2) : Color.clear)
        .cornerRadius(4)
        .contextMenu {
            Button {
                let tableInfo = "\(table.name)\t\(table.rowCount) rows\t\(table.formattedSize)"
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(tableInfo, forType: .string)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            
            Divider()
            
            Button(role: .destructive) {
                onDelete()
            } label: {
                Label("Drop Table...", systemImage: "trash")
            }
            .disabled(isProtected)
        }
    }
}

#Preview {
    SiteListView(viewModel: DatabaseBrowserViewModel())
        .frame(width: 280)
}
