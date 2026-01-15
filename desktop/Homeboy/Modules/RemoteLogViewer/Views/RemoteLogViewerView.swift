import SwiftUI

struct RemoteLogViewerView: View {
    @StateObject private var viewModel = RemoteLogViewerViewModel()
    @State private var showCopiedFeedback = false
    @State private var showBrowser = false

    var body: some View {
        logViewerContent
            .frame(minWidth: 700, minHeight: 400)
            .task {
                if viewModel.selectedLogId != nil {
                    await viewModel.fetchSelectedLog()
                }
            }
            .sheet(isPresented: $showBrowser) {
                RemoteFileBrowserView(
                    projectId: ConfigurationManager.shared.safeActiveProject.id,
                    startingPath: ConfigurationManager.shared.safeActiveProject.basePath,
                    mode: .selectFile,
                    onSelectPath: { path in
                        openLogFromBrowser(path)
                    }
                )
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
    
    // MARK: - Open Log from Browser
    
    private func openLogFromBrowser(_ path: String) {
        // Convert absolute path to relative from basePath
        let basePath = ConfigurationManager.shared.safeActiveProject.basePath ?? ""
        let relativePath = path.hasPrefix(basePath)
            ? String(path.dropFirst(basePath.count + 1))
            : path
        viewModel.openLog(path: relativePath)
    }
    
    // MARK: - Log Viewer Content
    
    private var logViewerContent: some View {
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
            onBrowse: {
                showBrowser = true
            }
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

            Text("Browse the server to select a log file")
                .font(.subheadline)
                .foregroundColor(.secondary)

            Button {
                showBrowser = true
            } label: {
                Label("Browse...", systemImage: "folder")
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
    
    private func errorView(_ error: any DisplayableError) -> some View {
        ErrorView(error) {
            Task { await viewModel.fetchSelectedLog() }
        }
    }
}

#Preview {
    RemoteLogViewerView()
}
