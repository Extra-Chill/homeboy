import AppKit
import SwiftUI

/// Sidebar view showing database tables as a flat list
struct SiteListView: View {
    @ObservedObject var viewModel: DatabaseBrowserViewModel

    var body: some View {
        List(viewModel.tables) { table in
            TableRowView(
                table: table,
                isSelected: viewModel.selectedTable?.id == table.id,
                onDelete: { viewModel.requestTableDeletion(table) }
            )
            .contentShape(Rectangle())
            .onTapGesture {
                Task {
                    await viewModel.selectTable(table)
                }
            }
        }
        .listStyle(.sidebar)
    }
}

/// Individual table row in the list
struct TableRowView: View {
    let table: DatabaseTable
    let isSelected: Bool
    let onDelete: () -> Void

    private var rowBackground: Color {
        isSelected ? Color.accentColor.opacity(0.2) : Color.clear
    }

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(table.name)
                    .font(.body)
                    .lineLimit(1)

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
        }
    }
}

#Preview {
    SiteListView(viewModel: DatabaseBrowserViewModel())
        .frame(width: 280)
}
