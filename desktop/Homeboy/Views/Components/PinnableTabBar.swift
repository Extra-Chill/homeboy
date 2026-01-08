import SwiftUI

/// Protocol for items that can be displayed in a PinnableTabBar
protocol PinnableTabItem: Identifiable where ID == UUID {
    var id: UUID { get }
    var displayName: String { get }
    var isPinned: Bool { get }
}

/// A reusable horizontal tab bar with pinnable tabs
/// Used by Remote File Editor and Remote Log Viewer
struct PinnableTabBar<Item: PinnableTabItem>: View {
    let items: [Item]
    let selectedId: UUID?
    let showIndicator: (Item) -> Bool  // e.g., unsaved changes indicator
    
    let onSelect: (UUID) -> Void
    let onClose: (UUID) -> Void
    let onPin: (UUID) -> Void
    let onUnpin: (UUID) -> Void
    let onBrowse: () -> Void
    
    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 0) {
                ForEach(items) { item in
                    tabButton(for: item)
                        .contextMenu {
                            tabContextMenu(for: item)
                        }
                }
                
                // Browse button
                Button {
                    onBrowse()
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "plus")
                            .font(.caption)
                        Text("Browse...")
                            .font(.subheadline)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
                
                Spacer()
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    private func tabButton(for item: Item) -> some View {
        Button {
            onSelect(item.id)
        } label: {
            HStack(spacing: 6) {
                // Pin indicator
                if item.isPinned {
                    Image(systemName: "pin.fill")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                
                Text(item.displayName)
                    .font(.subheadline)
                
                // Custom indicator (e.g., unsaved changes)
                if showIndicator(item) {
                    Circle()
                        .fill(Color.orange)
                        .frame(width: 6, height: 6)
                }
                
                // Close button (only for unpinned)
                if !item.isPinned {
                    Button {
                        onClose(item.id)
                    } label: {
                        Image(systemName: "xmark")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(item.id == selectedId ? Color.accentColor.opacity(0.2) : Color.clear)
            )
            .foregroundColor(item.id == selectedId ? .accentColor : .secondary)
        }
        .buttonStyle(.plain)
    }
    
    @ViewBuilder
    private func tabContextMenu(for item: Item) -> some View {
        if item.isPinned {
            Button {
                onUnpin(item.id)
            } label: {
                Label("Unpin", systemImage: "pin.slash")
            }
        } else {
            Button {
                onPin(item.id)
            } label: {
                Label("Pin", systemImage: "pin")
            }
        }
        
        Divider()
        
        Button(role: .destructive) {
            onClose(item.id)
        } label: {
            Label("Close", systemImage: "xmark")
        }
    }
}
