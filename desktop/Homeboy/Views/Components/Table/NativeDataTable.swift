import SwiftUI
import AppKit

/// Sort direction for NativeDataTable columns
enum DataTableSortDirection {
    case ascending
    case descending
    
    mutating func toggle() {
        self = (self == .ascending) ? .descending : .ascending
    }
}

/// Sort descriptor for NativeDataTable
struct DataTableSortDescriptor<Item> {
    let columnId: String
    var direction: DataTableSortDirection
    let comparator: (Item, Item) -> ComparisonResult
    
    func compare(_ lhs: Item, _ rhs: Item) -> ComparisonResult {
        let result = comparator(lhs, rhs)
        if direction == .descending {
            switch result {
            case .orderedAscending: return .orderedDescending
            case .orderedDescending: return .orderedAscending
            case .orderedSame: return .orderedSame
            }
        }
        return result
    }
}

/// Native macOS table component with full NSTableView behavior
struct NativeDataTable<Item: Identifiable>: NSViewRepresentable where Item.ID: Hashable {
    
    // Data
    let items: [Item]
    let columns: [DataTableColumn<Item>]
    
    // Selection
    @Binding var selection: Set<Item.ID>
    
    // Sorting
    @Binding var sortDescriptor: DataTableSortDescriptor<Item>?
    
    // Actions
    var onDoubleClick: ((Item) -> Void)?
    var onKeyboardActivate: ((Item) -> Void)?
    var contextMenuProvider: ((Set<Item.ID>) -> NSMenu?)?
    
    init(
        items: [Item],
        columns: [DataTableColumn<Item>],
        selection: Binding<Set<Item.ID>>,
        sortDescriptor: Binding<DataTableSortDescriptor<Item>?> = .constant(nil),
        onDoubleClick: ((Item) -> Void)? = nil,
        onKeyboardActivate: ((Item) -> Void)? = nil,
        contextMenuProvider: ((Set<Item.ID>) -> NSMenu?)? = nil
    ) {
        self.items = items
        self.columns = columns
        self._selection = selection
        self._sortDescriptor = sortDescriptor
        self.onDoubleClick = onDoubleClick
        self.onKeyboardActivate = onKeyboardActivate ?? onDoubleClick
        self.contextMenuProvider = contextMenuProvider
    }
    
    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        
        let tableView = ActivatableTableView()
        tableView.style = .plain
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.allowsMultipleSelection = true
        tableView.allowsEmptySelection = true
        tableView.allowsColumnReordering = false
        tableView.allowsColumnResizing = true
        tableView.allowsColumnSelection = false
        tableView.rowHeight = DataTableConstants.defaultRowHeight
        tableView.intercellSpacing = NSSize(width: 8, height: 0)
        tableView.gridStyleMask = []
        
        tableView.delegate = context.coordinator
        tableView.dataSource = context.coordinator
        
        // Double-click action
        tableView.target = context.coordinator
        tableView.doubleAction = #selector(Coordinator.tableViewDoubleClick(_:))
        
        // Keyboard activation (Enter/Return)
        tableView.onKeyboardActivate = { row in
            guard row >= 0, row < context.coordinator.parent.items.count else { return }
            let item = context.coordinator.parent.items[row]
            context.coordinator.parent.onKeyboardActivate?(item)
        }
        
        // Configure columns
        for column in columns {
            let tableColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            tableColumn.title = column.title
            tableColumn.headerCell.alignment = column.alignment
            
            // Configure width
            switch column.width {
            case .fixed(let width):
                tableColumn.width = width
                tableColumn.minWidth = width
                tableColumn.maxWidth = width
                tableColumn.resizingMask = []
            case .flexible(let min, let max):
                tableColumn.minWidth = min
                tableColumn.maxWidth = max
                tableColumn.resizingMask = NSTableColumn.ResizingOptions.userResizingMask
            case .auto(let min, let ideal, let max):
                tableColumn.width = ideal
                tableColumn.minWidth = min
                tableColumn.maxWidth = max
                tableColumn.resizingMask = NSTableColumn.ResizingOptions.userResizingMask
            }
            
            // Configure sorting
            if column.sortable, column.sortComparator != nil {
                tableColumn.sortDescriptorPrototype = NSSortDescriptor(key: column.id, ascending: true)
            }
            
            tableView.addTableColumn(tableColumn)
        }
        
        scrollView.documentView = tableView
        
        // Store reference for updates
        context.coordinator.tableView = tableView
        
        return scrollView
    }
    
    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let tableView = scrollView.documentView as? NSTableView else { return }
        
        context.coordinator.parent = self
        
        // Check if columns need to be rebuilt
        let currentColumnIds = tableView.tableColumns.map { $0.identifier.rawValue }
        let newColumnIds = columns.map { $0.id }
        
        if currentColumnIds != newColumnIds {
            rebuildColumns(tableView: tableView)
        }
        
        // Update data
        tableView.reloadData()
        
        // Sync selection from SwiftUI to NSTableView
        let newSelectedRows = IndexSet(items.enumerated().compactMap { index, item in
            selection.contains(item.id) ? index : nil
        })
        
        if tableView.selectedRowIndexes != newSelectedRows {
            context.coordinator.isUpdatingSelection = true
            tableView.selectRowIndexes(newSelectedRows, byExtendingSelection: false)
            context.coordinator.isUpdatingSelection = false
        }
        
        // Update sort indicator
        updateSortIndicator(tableView: tableView)
    }
    
    private func rebuildColumns(tableView: NSTableView) {
        // Remove all existing columns
        while let column = tableView.tableColumns.first {
            tableView.removeTableColumn(column)
        }
        
        // Add new columns
        for column in columns {
            let tableColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            tableColumn.title = column.title
            tableColumn.headerCell.alignment = column.alignment
            
            switch column.width {
            case .fixed(let width):
                tableColumn.width = width
                tableColumn.minWidth = width
                tableColumn.maxWidth = width
                tableColumn.resizingMask = []
            case .flexible(let min, let max):
                tableColumn.minWidth = min
                tableColumn.maxWidth = max
                tableColumn.resizingMask = NSTableColumn.ResizingOptions.userResizingMask
            case .auto(let min, let ideal, let max):
                tableColumn.width = ideal
                tableColumn.minWidth = min
                tableColumn.maxWidth = max
                tableColumn.resizingMask = NSTableColumn.ResizingOptions.userResizingMask
            }
            
            if column.sortable, column.sortComparator != nil {
                tableColumn.sortDescriptorPrototype = NSSortDescriptor(key: column.id, ascending: true)
            }
            
            tableView.addTableColumn(tableColumn)
        }
    }
    
    private func updateSortIndicator(tableView: NSTableView) {
        // Clear all indicators first
        for column in tableView.tableColumns {
            tableView.setIndicatorImage(nil, in: column)
        }
        
        // Set indicator for sorted column
        if let descriptor = sortDescriptor,
           let column = tableView.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(descriptor.columnId)) {
            let image = descriptor.direction == .ascending
                ? NSImage(systemSymbolName: "chevron.up", accessibilityDescription: "Ascending")
                : NSImage(systemSymbolName: "chevron.down", accessibilityDescription: "Descending")
            tableView.setIndicatorImage(image, in: column)
        }
    }
    
    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }
    
    // MARK: - Coordinator
    
    class Coordinator: NSObject, NSTableViewDelegate, NSTableViewDataSource {
        var parent: NativeDataTable
        weak var tableView: NSTableView?
        var isUpdatingSelection = false
        
        init(_ parent: NativeDataTable) {
            self.parent = parent
        }
        
        // MARK: - NSTableViewDataSource
        
        func numberOfRows(in tableView: NSTableView) -> Int {
            parent.items.count
        }
        
        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let columnId = tableColumn?.identifier.rawValue,
                  let column = parent.columns.first(where: { $0.id == columnId }),
                  row < parent.items.count else {
                return nil
            }
            
            let item = parent.items[row]
            return column.cellProvider(item)
        }
        
        func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            guard let sortDescriptor = tableView.sortDescriptors.first,
                  let columnId = sortDescriptor.key,
                  let column = parent.columns.first(where: { $0.id == columnId }),
                  let comparator = column.sortComparator else {
                parent.sortDescriptor = nil
                return
            }
            
            let direction: DataTableSortDirection = sortDescriptor.ascending ? .ascending : .descending
            parent.sortDescriptor = DataTableSortDescriptor(
                columnId: columnId,
                direction: direction,
                comparator: comparator
            )
        }
        
        // MARK: - NSTableViewDelegate
        
        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !isUpdatingSelection else { return }
            
            let selectedIndexes = tableView?.selectedRowIndexes ?? IndexSet()
            let selectedIds = Set(selectedIndexes.compactMap { index -> Item.ID? in
                guard index < parent.items.count else { return nil }
                return parent.items[index].id
            })
            
            if parent.selection != selectedIds {
                DispatchQueue.main.async {
                    self.parent.selection = selectedIds
                }
            }
        }
        
        func tableView(_ tableView: NSTableView, menuFor event: NSEvent, row: Int) -> NSMenu? {
            guard let provider = parent.contextMenuProvider else { return nil }
            
            // If clicking on an unselected row, select it first
            if row >= 0 && !tableView.selectedRowIndexes.contains(row) {
                tableView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            }
            
            let selectedIds = Set(tableView.selectedRowIndexes.compactMap { index -> Item.ID? in
                guard index < parent.items.count else { return nil }
                return parent.items[index].id
            })
            
            return provider(selectedIds)
        }
        
        // MARK: - Actions
        
        @objc func tableViewDoubleClick(_ sender: NSTableView) {
            let row = sender.clickedRow
            guard row >= 0, row < parent.items.count else { return }
            let item = parent.items[row]
            parent.onDoubleClick?(item)
        }
    }
}

// MARK: - Custom NSTableView with Keyboard Handling

private class ActivatableTableView: NSTableView {
    var onKeyboardActivate: ((Int) -> Void)?
    
    override func keyDown(with event: NSEvent) {
        // Enter or Return key activates the selected row
        if event.keyCode == 36 || event.keyCode == 76 { // Return or Enter
            if selectedRow >= 0 {
                onKeyboardActivate?(selectedRow)
                return
            }
        }
        super.keyDown(with: event)
    }
}

// MARK: - NSTableViewDelegate Extension for Context Menu

extension NSTableViewDelegate {
    func tableView(_ tableView: NSTableView, menuFor event: NSEvent, row: Int) -> NSMenu? {
        return nil
    }
}
