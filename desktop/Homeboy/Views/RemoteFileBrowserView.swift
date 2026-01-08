import SwiftUI

/// View for browsing remote server filesystem
struct RemoteFileBrowserView: View {
    @StateObject private var browser: RemoteFileBrowser
    @Environment(\.dismiss) private var dismiss
    
    let mode: FileBrowserMode
    let onSelectPath: ((String) -> Void)?
    
    init(serverId: String, mode: FileBrowserMode = .browse, onSelectPath: ((String) -> Void)? = nil) {
        _browser = StateObject(wrappedValue: RemoteFileBrowser(serverId: serverId))
        self.mode = mode
        self.onSelectPath = onSelectPath
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Toolbar
            toolbar
            
            Divider()
            
            // Breadcrumbs
            breadcrumbsBar
            
            Divider()
            
            // Content
            content
            
            // Footer (for selectPath mode)
            if mode == .selectPath {
                Divider()
                selectionFooter
            }
        }
        .frame(minWidth: 500, minHeight: 400)
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
            VStack(spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.largeTitle)
                    .foregroundColor(.orange)
                Text(error)
                    .foregroundColor(.secondary)
                Button("Retry") {
                    Task { await browser.refresh() }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if browser.entries.isEmpty && !browser.isLoading {
            VStack(spacing: 12) {
                Image(systemName: "folder")
                    .font(.largeTitle)
                    .foregroundColor(.secondary)
                Text("Directory is empty")
                    .foregroundColor(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            List(browser.entries) { entry in
                FileEntryRow(entry: entry, isLoading: browser.isLoading) {
                    Task { await browser.navigateInto(entry) }
                }
            }
            .listStyle(.plain)
        }
    }
    
    // MARK: - Selection Footer
    
    private var selectionFooter: some View {
        HStack {
            Text("Selected: ")
                .foregroundColor(.secondary)
            Text(browser.currentPath)
                .fontWeight(.medium)
                .lineLimit(1)
                .truncationMode(.middle)
            
            Spacer()
            
            Button("Cancel") {
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            
            Button("Select") {
                onSelectPath?(browser.currentPath)
                dismiss()
            }
            .keyboardShortcut(.defaultAction)
            .disabled(browser.currentPath.isEmpty)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }
}

// MARK: - File Entry Row

private struct FileEntryRow: View {
    let entry: RemoteFileEntry
    let isLoading: Bool
    let onNavigate: () -> Void
    
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: entry.icon)
                .foregroundColor(entry.isDirectory ? .accentColor : .secondary)
                .frame(width: 20)
            
            Text(entry.name)
                .lineLimit(1)
            
            Spacer()
            
            if let size = entry.formattedSize {
                Text(size)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            if let permissions = entry.permissions {
                Text(permissions)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fontDesign(.monospaced)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) {
            if entry.isDirectory {
                onNavigate()
            }
        }
        .opacity(isLoading ? 0.5 : 1.0)
    }
}

// MARK: - Preview

#Preview {
    RemoteFileBrowserView(serverId: "test", mode: .browse)
}
