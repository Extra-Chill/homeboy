import SwiftUI

struct RemoteLogViewerView: View {
    @StateObject private var viewModel = RemoteLogViewerViewModel()
    @State private var showCopiedFeedback = false
    
    var body: some View {
        VStack(spacing: 0) {
            tabBar
            Divider()
            
            if viewModel.openLogs.isEmpty {
                emptyState
            } else if let log = viewModel.selectedLog {
                headerSection(log: log)
                Divider()
                toolbarSection(log: log)
                Divider()
                logContentSection(log: log)
            }
        }
        .frame(minWidth: 600, minHeight: 400)
        .task {
            if viewModel.selectedLogId != nil {
                await viewModel.fetchSelectedLog()
            }
        }
        .sheet(isPresented: $viewModel.showFileBrowser) {
            if let serverId = viewModel.serverId {
                RemoteFileBrowserView(
                    serverId: serverId,
                    startingPath: ConfigurationManager.shared.safeActiveProject.basePath,
                    mode: .selectFile
                ) { selectedPath in
                    // Extract relative path from basePath
                    let basePath = ConfigurationManager.shared.safeActiveProject.basePath ?? ""
                    let relativePath = selectedPath.hasPrefix(basePath)
                        ? String(selectedPath.dropFirst(basePath.count + 1))
                        : selectedPath
                    viewModel.openLog(path: relativePath)
                }
            }
        }
        .alert("Clear Log", isPresented: $viewModel.showClearConfirmation) {
            Button("Cancel", role: .cancel) {}
            Button("Clear", role: .destructive) {
                Task { await viewModel.clearSelectedLog() }
            }
        } message: {
            if let log = viewModel.selectedLog {
                Text("This will permanently delete the contents of \(log.displayName). This cannot be undone.")
            }
        }
    }
    
    // MARK: - Tab Bar
    
    private var tabBar: some View {
        PinnableTabBar(
            items: viewModel.openLogs,
            selectedId: viewModel.selectedLogId,
            showIndicator: { _ in false },  // No indicator for logs
            onSelect: { viewModel.selectLog($0) },
            onClose: { viewModel.closeLog($0) },
            onPin: { viewModel.pinLog($0) },
            onUnpin: { viewModel.unpinLog($0) },
            onBrowse: { viewModel.showFileBrowser = true }
        )
    }
    
    // MARK: - Empty State
    
    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "doc.text.magnifyingglass")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("No Logs Open")
                .font(.headline)
            
            Text("Click \"Browse...\" to open a log file from the server")
                .font(.subheadline)
                .foregroundColor(.secondary)
            
            Button {
                viewModel.showFileBrowser = true
            } label: {
                Label("Browse Files", systemImage: "folder")
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    // MARK: - Header
    
    private func headerSection(log: OpenLog) -> some View {
        HStack {
            if log.isPinned {
                Image(systemName: "pin.fill")
                    .foregroundColor(.secondary)
                    .font(.caption)
            }
            
            Text(log.path)
                .font(.headline)
            
            Spacer()
            
            Text("Fetched: \(viewModel.lastFetchedFormatted(for: log))")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding()
    }
    
    // MARK: - Toolbar
    
    private func toolbarSection(log: OpenLog) -> some View {
        HStack(spacing: 12) {
            // Tail lines picker
            Picker("Lines", selection: Binding(
                get: { log.tailLines },
                set: { viewModel.setTailLines($0) }
            )) {
                ForEach(RemoteLogViewerViewModel.tailOptions, id: \.self) { count in
                    Text(count == 0 ? "All" : "\(count)").tag(count)
                }
            }
            .frame(width: 100)
            
            // Refresh
            Button {
                Task { await viewModel.fetchSelectedLog() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(viewModel.isLoading)
            
            Spacer()
            
            // Copy
            Button {
                viewModel.copyContent()
                showCopiedFeedback = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    showCopiedFeedback = false
                }
            } label: {
                Image(systemName: showCopiedFeedback ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(.borderless)
            .disabled(log.content.isEmpty)
            .help("Copy to clipboard")
            
            // Clear log
            Button {
                viewModel.showClearConfirmation = true
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .disabled(viewModel.isLoading || !log.fileExists)
            .help("Clear log file")
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Log Content Section
    
    @ViewBuilder
    private func logContentSection(log: OpenLog) -> some View {
        if viewModel.isLoading && log.content.isEmpty {
            loadingView(log: log)
        } else if let error = viewModel.error {
            errorView(error)
        } else if !log.fileExists {
            LogEmptyView(fileName: log.displayName, fileExists: false)
        } else if log.content.isEmpty {
            LogEmptyView(fileName: log.displayName, fileExists: true)
        } else {
            LogContentView(content: log.content, isLoading: viewModel.isLoading)
        }
    }
    
    private func loadingView(log: OpenLog) -> some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading \(log.displayName)...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func errorView(_ error: AppError) -> some View {
        ErrorView(error) {
            Task { await viewModel.fetchSelectedLog() }
        }
    }
}

#Preview {
    RemoteLogViewerView()
}
