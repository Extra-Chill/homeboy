import SwiftUI

/// A submenu for adding items to an existing group or creating a new group
struct AddToGroupMenu: View {
    let availableGroupings: [ItemGrouping]
    let onAddToGroup: (ItemGrouping) -> Void
    let onCreateNewGroup: () -> Void
    
    var body: some View {
        Menu("Add to Group...") {
            ForEach(availableGroupings) { grouping in
                Button(grouping.name) {
                    onAddToGroup(grouping)
                }
            }
            
            if !availableGroupings.isEmpty {
                Divider()
            }
            
            Button("New Group...") {
                onCreateNewGroup()
            }
        }
    }
}

/// Context menu items for managing a grouping (for group headers)
struct GroupingContextMenuItems: View {
    let grouping: ItemGrouping
    let canMoveUp: Bool
    let canMoveDown: Bool
    let onRename: () -> Void
    let onMoveUp: () -> Void
    let onMoveDown: () -> Void
    let onDelete: () -> Void
    
    var body: some View {
        Button("Rename Group...") {
            onRename()
        }
        
        Divider()
        
        Button("Move Up") {
            onMoveUp()
        }
        .disabled(!canMoveUp)
        
        Button("Move Down") {
            onMoveDown()
        }
        .disabled(!canMoveDown)
        
        Divider()
        
        Button("Delete Group", role: .destructive) {
            onDelete()
        }
    }
}

/// Context menu items for a table row in database browser
struct TableRowContextMenuItems: View {
    let table: DatabaseTable
    let isProtected: Bool
    let isInGroup: Bool
    let availableGroupings: [ItemGrouping]
    let onCopy: () -> Void
    let onAddToGroup: (ItemGrouping) -> Void
    let onCreateNewGroup: () -> Void
    let onRemoveFromGroup: () -> Void
    let onProtect: () -> Void
    let onUnprotect: () -> Void
    let onUnlock: () -> Void
    let onDropTable: () -> Void
    let isCoreProtected: Bool
    let isUnlocked: Bool
    
    var body: some View {
        Button {
            onCopy()
        } label: {
            Label("Copy", systemImage: "doc.on.doc")
        }
        
        Divider()
        
        // Group management
        Menu("Add to Group...") {
            ForEach(availableGroupings) { grouping in
                Button(grouping.name) {
                    onAddToGroup(grouping)
                }
            }
            
            if !availableGroupings.isEmpty {
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
                        // Lock is inverse of unlock
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
        
        Button(role: .destructive) {
            onDropTable()
        } label: {
            Label("Drop Table...", systemImage: "trash")
        }
        .disabled(isProtected && !isUnlocked)
    }
}
