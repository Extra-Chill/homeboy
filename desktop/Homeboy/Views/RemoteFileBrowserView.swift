import SwiftUI
import AppKit

/// View for browsing remote server filesystem
struct RemoteFileBrowserView: View {
    @StateObject private var browser: RemoteFileBrowser
    @Environment(\.dismiss) private var dismiss
    
    let mode: FileBrowserMode
    let onSelectPath: ((String) -> Void)?
    
    @State private var sortDescriptor: DataTableSortDescriptor<RemoteFileEntry>?
    
    init(serverId: String, startingPath: String? = nil, mode: FileBrowserMode = .browse, onSelectPath: ((String) -> Void)? = nil) {
        _browser = StateObject(wrappedValue: RemoteFileBrowser(serverId: serverId, startingPath: startingPath))
        self.mode = mode
        self.onSelectPath = onSelectPath
    }
    
    /// Sorted entries based on current sort descriptor, with default sort (directories first, then alphabetical)
    private var sortedEntries: [RemoteFileEntry] {
        if let descriptor = sortDescriptor {
            return browser.entries.sorted { lhs, rhs in
                descriptor.compare(lhs, rhs) == .orderedAscending
            }
        }
        // Default: directories first, then alphabetical
        return browser.entries.sorted()
    }
    
    var body: some View {
        VStack(spacing: 0) {
            toolbar
            
            Divider()
            
            breadcrumbsBar
            
            Divider()
            
            content
            
            if mode == .selectPath || mode == .selectFile {
                Divider()
                selectionFooter
            }
        }
        .frame(minWidth: 600, minHeight: 400)
        .task {
            await browser.connect()
        }
    }
    
    // MARK: - Toolbar
    
    private var toolbar: some View {
        HStack(spacing: 12) {
            Button(action: { Task { await browser.goBack() } }) {
                Image(systemName: "chevron.left")
            }
            .disabled(!browser.canGoBack || browser.isLoading)
            .help("Go back")
            
            Button(action: { Task { await browser.goUp() } }) {
                Image(systemName: "chevron.up")
            }
            .disabled(!browser.canGoUp || browser.isLoading)
            .help("Go to parent directory")
            
            Button(action: { Task { await browser.goToHome() } }) {
                Image(systemName: "house")
            }
            .disabled(browser.isLoading)
            .help("Go to home directory")
            
            Divider()
                .frame(height: 16)
            
            Button(action: { Task { await browser.refresh() } }) {
                Image(systemName: "arrow.clockwise")
            }
            .disabled(browser.isLoading)
            .help("Refresh")
            
            Spacer()
            
            if browser.isLoading {
                ProgressView()
                    .controlSize(.small)
            }
            
            if mode == .browse {
                Button("Done") {
                    dismiss()
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .buttonStyle(.borderless)
    }
    
    // MARK: - Breadcrumbs
    
    private var breadcrumbsBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                Button(action: { Task { await browser.goToPath("/") } }) {
                    Image(systemName: "externaldrive.fill")
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
                    .disabled(browser.isLoading)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    // MARK: - Content
    
    @ViewBuilder
    private var content: some View {
        if let error = browser.error {
            ErrorView(error) {
                Task { await browser.refresh() }
            }
        } else if browser.entries.isEmpty && !browser.isLoading {
            Color.clear
        } else {
            fileTable
        }
    }
    
    // MARK: - File Table
    
    private var fileTable: some View {
        NativeDataTable(
            items: sortedEntries,
            columns: fileTableColumns,
            selection: $browser.selectedEntries,
            sortDescriptor: $sortDescriptor,
            onDoubleClick: { entry in
                if entry.isDirectory {
                    Task { await browser.navigateInto(entry) }
                }
            },
            onKeyboardActivate: { entry in
                if entry.isDirectory {
                    Task { await browser.navigateInto(entry) }
                }
            }
        )
    }
    
    private var fileTableColumns: [DataTableColumn<RemoteFileEntry>] {
        [
            .iconWithText(
                id: "name",
                title: "Name",
                width: .auto(min: 200, ideal: 300, max: 600),
                textKeyPath: \.name,
                iconKeyPath: \.icon,
                iconColorProvider: { entry in
                    entry.isDirectory ? NSColor.controlAccentColor : NSColor.secondaryLabelColor
                }
            ),
            .monospaced(
                id: "size",
                title: "Size",
                width: .auto(min: 60, ideal: 80, max: 120),
                alignment: .right,
                keyPath: \.formattedSize,
                nullPlaceholder: "—"
            ),
            .monospaced(
                id: "permissions",
                title: "Permissions",
                width: .auto(min: 80, ideal: 100, max: 120),
                alignment: .left,
                keyPath: \.permissions,
                nullPlaceholder: ""
            )
        ]
    }
    
    // MARK: - Selection Footer
    
    private var selectedPath: String {
        if let file = browser.selectedFile {
            // For selectFile: always use the highlighted file
            // For selectPath: use highlighted directory if one is selected
            if mode == .selectFile || (mode == .selectPath && file.isDirectory) {
                return browser.currentPath.hasSuffix("/")
                    ? "\(browser.currentPath)\(file.name)"
                    : "\(browser.currentPath)/\(file.name)"
            }
        }
        return browser.currentPath
    }
    
    private var canSelect: Bool {
        switch mode {
        case .selectPath:
            return !browser.currentPath.isEmpty
        case .selectFile:
            return browser.selectedFile != nil && !(browser.selectedFile?.isDirectory ?? true)
        case .browse:
            return false
        }
    }
    
    private var selectionFooter: some View {
        HStack {
            Text("Selected: ")
                .foregroundColor(.secondary)
            Text(selectedPath)
                .fontWeight(.medium)
                .lineLimit(1)
                .truncationMode(.middle)
            
            Spacer()
            
            Button("Cancel") {
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            
            Button("Select") {
                onSelectPath?(selectedPath)
                dismiss()
            }
            .keyboardShortcut(.defaultAction)
            .disabled(!canSelect)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }
}

// MARK: - Preview

#Preview {
    RemoteFileBrowserView(serverId: "test", mode: .browse)
}
