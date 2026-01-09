import SwiftUI
import AppKit

/// Reusable file browser sidebar for Remote File Editor and Log Viewer
///
/// Provides a compact file tree view with navigation and optional file operations.
/// When `fileOperationsEnabled` is true, users can create, rename, and delete files/folders.
struct FileBrowserSidebarView: View {
    @ObservedObject var browser: RemoteFileBrowser
    let onFileSelected: (String) -> Void
    let onCollapse: () -> Void
    let fileOperationsEnabled: Bool
    
    // Callbacks for file operations (used to sync editor tabs)
    let onFileDeleted: ((String) -> Void)?
    let onFileRenamed: ((String, String) -> Void)?  // (oldPath, newPath)
    
    // MARK: - Local State
    
    @State private var showNewFileSheet = false
    @State private var showNewFolderSheet = false
    @State private var showRenameSheet = false
    @State private var showDeleteConfirmation = false
    @State private var newItemName = ""
    @State private var entryToRename: RemoteFileEntry?
    @State private var entryToDelete: RemoteFileEntry?
    @State private var operationError: AppError?
    @State private var sortDescriptor: DataTableSortDescriptor<RemoteFileEntry>?
    
    init(
        browser: RemoteFileBrowser,
        onFileSelected: @escaping (String) -> Void,
        onCollapse: @escaping () -> Void,
        fileOperationsEnabled: Bool = true,
        onFileDeleted: ((String) -> Void)? = nil,
        onFileRenamed: ((String, String) -> Void)? = nil
    ) {
        self.browser = browser
        self.onFileSelected = onFileSelected
        self.onCollapse = onCollapse
        self.fileOperationsEnabled = fileOperationsEnabled
        self.onFileDeleted = onFileDeleted
        self.onFileRenamed = onFileRenamed
    }
    
    /// Sorted entries based on current sort descriptor, with default sort (directories first, then alphabetical)
    private var sortedEntries: [RemoteFileEntry] {
        if let descriptor = sortDescriptor {
            return browser.entries.sorted { lhs, rhs in
                descriptor.compare(lhs, rhs) == .orderedAscending
            }
        }
        return browser.entries.sorted()
    }
    
    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            breadcrumbs
            Divider()
            
            if let error = browser.error {
                errorSection(error)
            } else if browser.isLoading && browser.entries.isEmpty {
                loadingSection
            } else {
                fileList
            }
        }
        .sheet(isPresented: $showNewFileSheet) {
            newItemSheet(title: "New File", placeholder: "filename.txt") { name in
                Task { await createFile(named: name) }
            }
        }
        .sheet(isPresented: $showNewFolderSheet) {
            newItemSheet(title: "New Folder", placeholder: "folder-name") { name in
                Task { await createFolder(named: name) }
            }
        }
        .sheet(isPresented: $showRenameSheet) {
            if let entry = entryToRename {
                renameSheet(entry: entry)
            }
        }
        .alert("Delete", isPresented: $showDeleteConfirmation) {
            deleteConfirmationButtons
        } message: {
            deleteConfirmationMessage
        }
        .alert("Error", isPresented: .init(
            get: { operationError != nil },
            set: { if !$0 { operationError = nil } }
        )) {
            Button("OK") { operationError = nil }
        } message: {
            if let error = operationError {
                Text(error.body)
            }
        }
    }
    
    // MARK: - Toolbar
    
    private var toolbar: some View {
        HStack(spacing: 6) {
            // Navigation buttons
            Button { Task { await browser.goBack() } } label: {
                Image(systemName: "chevron.left")
            }
            .disabled(!browser.canGoBack || browser.isLoading)
            .help("Go back")
            
            Button { Task { await browser.goUp() } } label: {
                Image(systemName: "chevron.up")
            }
            .disabled(!browser.canGoUp || browser.isLoading)
            .help("Go to parent directory")
            
            Button { Task { await browser.goToHome() } } label: {
                Image(systemName: "house")
            }
            .disabled(browser.isLoading)
            .help("Go to home directory")
            
            Button { Task { await browser.refresh() } } label: {
                Image(systemName: "arrow.clockwise")
            }
            .disabled(browser.isLoading)
            .help("Refresh")
            
            Spacer()
            
            if browser.isLoading {
                ProgressView()
                    .controlSize(.small)
                    .scaleEffect(0.7)
            }
            
            // New file/folder menu (if enabled)
            if fileOperationsEnabled {
                Menu {
                    Button("New File...") {
                        newItemName = ""
                        showNewFileSheet = true
                    }
                    Button("New Folder...") {
                        newItemName = ""
                        showNewFolderSheet = true
                    }
                } label: {
                    Image(systemName: "plus")
                }
                .menuStyle(.borderlessButton)
                .frame(width: 20)
                .disabled(browser.isLoading || browser.currentPath.isEmpty)
                .help("Create new file or folder")
            }
            
            // Collapse button
            Button { onCollapse() } label: {
                Image(systemName: "sidebar.left")
            }
            .help("Hide sidebar")
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
    }
    
    // MARK: - Breadcrumbs
    
    private var breadcrumbs: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                Button { Task { await browser.goToPath("/") } } label: {
                    Image(systemName: "externaldrive.fill")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .disabled(browser.isLoading)
                
                ForEach(browser.breadcrumbs, id: \.path) { crumb in
                    Image(systemName: "chevron.right")
                        .foregroundColor(.secondary)
                        .font(.caption2)
                    
                    Button(crumb.name) {
                        Task { await browser.goToPath(crumb.path) }
                    }
                    .buttonStyle(.borderless)
                    .font(.caption)
                    .disabled(browser.isLoading)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
        }
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    // MARK: - File List
    
    private var fileList: some View {
        NativeDataTable(
            items: sortedEntries,
            columns: fileColumns,
            selection: $browser.selectedEntries,
            sortDescriptor: $sortDescriptor,
            onDoubleClick: { entry in
                if entry.isDirectory {
                    Task { await browser.navigateInto(entry) }
                } else {
                    onFileSelected(entry.path)
                }
            },
            onKeyboardActivate: { entry in
                if entry.isDirectory {
                    Task { await browser.navigateInto(entry) }
                } else {
                    onFileSelected(entry.path)
                }
            },
            contextMenuProvider: fileOperationsEnabled ? { [self] selectedIds in
                createContextMenu(for: selectedIds)
            } : nil
        )
    }
    
    private var fileColumns: [DataTableColumn<RemoteFileEntry>] {
        [
            .iconWithText(
                id: "name",
                title: "Name",
                width: .auto(min: 120, ideal: 200, max: 400),
                textKeyPath: \.name,
                iconKeyPath: \.icon,
                iconColorProvider: { entry in
                    entry.isDirectory ? NSColor.controlAccentColor : NSColor.secondaryLabelColor
                }
            ),
            .monospaced(
                id: "size",
                title: "Size",
                width: .auto(min: 50, ideal: 60, max: 80),
                alignment: .right,
                keyPath: \.formattedSize,
                nullPlaceholder: "-"
            )
        ]
    }
    
    // MARK: - Context Menu
    
    private func createContextMenu(for selectedIds: Set<String>) -> NSMenu? {
        guard selectedIds.count == 1,
              let entryId = selectedIds.first,
              let entry = browser.entries.first(where: { $0.id == entryId }) else {
            return nil
        }
        
        let menu = NSMenu()
        
        // Open action
        if entry.isDirectory {
            menu.addItem(makeMenuItem(title: "Open") { [browser] in
                Task { await browser.navigateInto(entry) }
            })
        } else {
            menu.addItem(makeMenuItem(title: "Open in Editor") { [onFileSelected] in
                onFileSelected(entry.path)
            })
        }
        
        menu.addItem(NSMenuItem.separator())
        
        // Rename - capture the entry to trigger the sheet
        let entryForRename = entry
        menu.addItem(makeMenuItem(title: "Rename...") {
            DispatchQueue.main.async { [self] in
                entryToRename = entryForRename
                newItemName = entryForRename.name
                showRenameSheet = true
            }
        })
        
        // Delete - capture the entry to trigger confirmation
        let entryForDelete = entry
        menu.addItem(makeMenuItem(title: "Delete") {
            DispatchQueue.main.async { [self] in
                entryToDelete = entryForDelete
                showDeleteConfirmation = true
            }
        })
        
        return menu
    }
    
    /// Helper to create menu items with closure-based actions
    private func makeMenuItem(title: String, action: @escaping () -> Void) -> NSMenuItem {
        let item = ClosureMenuItem(title: title, action: action)
        return item
    }
    
    // MARK: - Error / Loading States
    
    private func errorSection(_ error: AppError) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.title)
                .foregroundColor(.orange)
            
            Text(error.body)
                .font(.caption)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
            
            Button("Retry") {
                Task { await browser.refresh() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var loadingSection: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading...")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    // MARK: - New Item Sheet
    
    private func newItemSheet(title: String, placeholder: String, onCreate: @escaping (String) -> Void) -> some View {
        VStack(spacing: 16) {
            Text(title)
                .font(.headline)
            
            TextField(placeholder, text: $newItemName)
                .textFieldStyle(.roundedBorder)
                .frame(width: 250)
            
            HStack(spacing: 12) {
                Button("Cancel") {
                    showNewFileSheet = false
                    showNewFolderSheet = false
                }
                .keyboardShortcut(.cancelAction)
                
                Button("Create") {
                    onCreate(newItemName)
                    showNewFileSheet = false
                    showNewFolderSheet = false
                }
                .keyboardShortcut(.defaultAction)
                .disabled(newItemName.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
    }
    
    // MARK: - Rename Sheet
    
    private func renameSheet(entry: RemoteFileEntry) -> some View {
        VStack(spacing: 16) {
            Text("Rename")
                .font(.headline)
            
            TextField(entry.name, text: $newItemName)
                .textFieldStyle(.roundedBorder)
                .frame(width: 250)
                .onAppear {
                    newItemName = entry.name
                }
            
            HStack(spacing: 12) {
                Button("Cancel") {
                    showRenameSheet = false
                    entryToRename = nil
                }
                .keyboardShortcut(.cancelAction)
                
                Button("Rename") {
                    Task { await renameEntry(entry, to: newItemName) }
                    showRenameSheet = false
                    entryToRename = nil
                }
                .keyboardShortcut(.defaultAction)
                .disabled(newItemName.trimmingCharacters(in: .whitespaces).isEmpty || newItemName == entry.name)
            }
        }
        .padding(20)
    }
    
    // MARK: - Delete Confirmation
    
    @ViewBuilder
    private var deleteConfirmationButtons: some View {
        Button("Cancel", role: .cancel) {
            entryToDelete = nil
        }
        Button("Delete", role: .destructive) {
            if let entry = entryToDelete {
                Task { await deleteEntry(entry) }
            }
            entryToDelete = nil
        }
    }
    
    @ViewBuilder
    private var deleteConfirmationMessage: some View {
        if let entry = entryToDelete {
            if entry.isDirectory {
                Text("Delete folder \"\(entry.name)\" and all its contents? This cannot be undone.")
            } else {
                Text("Delete \"\(entry.name)\"? This cannot be undone.")
            }
        }
    }
    
    // MARK: - File Operations
    
    private func createFile(named name: String) async {
        do {
            let path = try await browser.createFile(named: name)
            onFileSelected(path)
        } catch {
            operationError = AppError("Failed to create file: \(error.localizedDescription)", source: "File Browser")
        }
    }
    
    private func createFolder(named name: String) async {
        do {
            _ = try await browser.createDirectory(named: name)
        } catch {
            operationError = AppError("Failed to create folder: \(error.localizedDescription)", source: "File Browser")
        }
    }
    
    private func renameEntry(_ entry: RemoteFileEntry, to newName: String) async {
        let oldPath = entry.path
        do {
            let newPath = try await browser.renameEntry(entry, newName: newName)
            onFileRenamed?(oldPath, newPath)
        } catch {
            operationError = AppError("Failed to rename: \(error.localizedDescription)", source: "File Browser")
        }
    }
    
    private func deleteEntry(_ entry: RemoteFileEntry) async {
        let path = entry.path
        do {
            try await browser.deleteEntry(entry)
            onFileDeleted?(path)
        } catch {
            operationError = AppError("Failed to delete: \(error.localizedDescription)", source: "File Browser")
        }
    }
}

// MARK: - Closure-based NSMenuItem

/// NSMenuItem subclass that executes a closure when activated
private class ClosureMenuItem: NSMenuItem {
    private var actionClosure: (() -> Void)?
    
    init(title: String, action: @escaping () -> Void) {
        self.actionClosure = action
        super.init(title: title, action: #selector(performAction), keyEquivalent: "")
        self.target = self
    }
    
    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }
    
    @objc private func performAction() {
        actionClosure?()
    }
}

// MARK: - Preview

#Preview {
    struct PreviewWrapper: View {
        var body: some View {
            FileBrowserSidebarView(
                browser: RemoteFileBrowser(serverId: "test"),
                onFileSelected: { path in print("Selected: \(path)") },
                onCollapse: { print("Collapse") },
                fileOperationsEnabled: true,
                onFileDeleted: { path in print("Deleted: \(path)") },
                onFileRenamed: { old, new in print("Renamed: \(old) -> \(new)") }
            )
            .frame(width: 280, height: 500)
        }
    }
    return PreviewWrapper()
}
