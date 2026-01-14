import SwiftUI

struct RemoteFileEditorView: View {
    @StateObject private var viewModel = RemoteFileEditorViewModel()
    @StateObject private var browser: RemoteFileBrowser
    @State private var showCopiedFeedback = false
    
    init() {
        let serverId = ConfigurationManager.shared.safeActiveProject.serverId ?? ""
        let basePath = ConfigurationManager.shared.safeActiveProject.basePath
        _browser = StateObject(wrappedValue: RemoteFileBrowser(projectId: serverId, startingPath: basePath))
    }
    
    var body: some View {
        CollapsibleSplitView(
            orientation: .horizontal,
            collapseSide: .leading,
            isCollapsed: $viewModel.sidebarCollapsed,
            panelSize: (min: 200, ideal: 260, max: 400)
        ) {
            // Primary content: Editor
            editorContent
        } secondary: {
            // Sidebar: File browser
            FileBrowserSidebarView(
                browser: browser,
                onFileSelected: { path in
                    openFileFromBrowser(path)
                },
                onCollapse: {
                    viewModel.sidebarCollapsed = true
                },
                fileOperationsEnabled: true,
                onFileDeleted: { path in
                    viewModel.handleFileDeleted(path)
                },
                onFileRenamed: { oldPath, newPath in
                    viewModel.handleFileRenamed(from: oldPath, to: newPath)
                }
            )
        }
        .frame(minWidth: 700, minHeight: 400)
        .task {
            await browser.connect()
            if viewModel.selectedFileId != nil {
                await viewModel.fetchSelectedFile()
            }
        }
        .alert("Unsaved Changes", isPresented: $viewModel.showCloseConfirmation) {
            Button("Cancel", role: .cancel) {
                viewModel.pendingCloseFileId = nil
            }
            Button("Discard", role: .destructive) {
                viewModel.confirmClose()
            }
        } message: {
            Text("You have unsaved changes. Discard them?")
        }
        .alert("Confirm Save", isPresented: $viewModel.showSaveConfirmation) {
            Button("Cancel", role: .cancel) {}
            Button("Save") {
                Task { await viewModel.saveSelectedFile() }
            }
        } message: {
            if let file = viewModel.selectedFile {
                Text("This will overwrite \(file.displayName) on the server.")
            }
        }
    }
    
    // MARK: - Open File from Browser
    
    private func openFileFromBrowser(_ path: String) {
        // Convert absolute path to relative from basePath
        let basePath = ConfigurationManager.shared.safeActiveProject.basePath ?? ""
        let relativePath = path.hasPrefix(basePath)
            ? String(path.dropFirst(basePath.count + 1))
            : path
        viewModel.openFile(path: relativePath)
    }
    
    // MARK: - Editor Content
    
    private var editorContent: some View {
        VStack(spacing: 0) {
            tabBar
            Divider()
            
            if viewModel.openFiles.isEmpty {
                emptyState
            } else if let file = viewModel.selectedFile {
                headerSection(file: file)
                Divider()
                toolbarSection(file: file)
                Divider()
                editorSection(file: file)
            }
        }
    }
    
    // MARK: - Tab Bar
    
    private var tabBar: some View {
        PinnableTabBar(
            items: viewModel.openFiles,
            selectedId: viewModel.selectedFileId,
            showIndicator: { $0.hasUnsavedChanges },
            onSelect: { viewModel.selectFile($0) },
            onClose: { viewModel.closeFile($0) },
            onPin: { viewModel.pinFile($0) },
            onUnpin: { viewModel.unpinFile($0) }
        )
    }
    
    // MARK: - Empty State
    
    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "doc.text")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("No Files Open")
                .font(.headline)
            
            if viewModel.sidebarCollapsed {
                Text("Use the sidebar to browse and open files")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
                
                Button {
                    viewModel.sidebarCollapsed = false
                } label: {
                    Label("Show Sidebar", systemImage: "sidebar.left")
                }
                .buttonStyle(.borderedProminent)
            } else {
                Text("Select a file from the sidebar to edit")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    // MARK: - Header
    
    private func headerSection(file: OpenFile) -> some View {
        HStack {
            if file.isPinned {
                Image(systemName: "pin.fill")
                    .foregroundColor(.secondary)
                    .font(.caption)
            }

            Text(file.path)
                .font(.headline)

            if !file.formattedSize.isEmpty {
                Text(file.formattedSize)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if file.hasUnsavedChanges {
                Text("Modified")
                    .font(.caption)
                    .foregroundColor(.orange)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Color.orange.opacity(0.1))
                    .cornerRadius(4)
            }

            Text("Fetched: \(viewModel.lastFetchedFormatted(for: file))")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding()
    }
    
    // MARK: - Toolbar
    
    private func toolbarSection(file: OpenFile) -> some View {
        HStack(spacing: 12) {
            // Refresh
            Button {
                Task { await viewModel.fetchSelectedFile() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(viewModel.isLoading || viewModel.isSaving)
            
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
            .disabled(file.content.isEmpty)
            .help("Copy to clipboard")
            
            // Save
            Button {
                viewModel.showSaveConfirmation = true
            } label: {
                HStack(spacing: 4) {
                    Label("Save", systemImage: "square.and.arrow.up")
                    if file.hasUnsavedChanges {
                        Circle()
                            .fill(Color.orange)
                            .frame(width: 6, height: 6)
                    }
                }
            }
            .disabled(!file.hasUnsavedChanges || viewModel.isLoading || viewModel.isSaving)
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Editor Section
    
    @ViewBuilder
    private func editorSection(file: OpenFile) -> some View {
        if viewModel.isLoading && file.content.isEmpty {
            loadingView(file: file)
        } else if let error = viewModel.error {
            errorView(error)
        } else if !file.fileExists {
            fileNotFoundView(file: file)
        } else {
            editorView(file: file)
        }
    }
    
    private func loadingView(file: OpenFile) -> some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading \(file.displayName)...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func errorView(_ error: any DisplayableError) -> some View {
        ErrorView(error) {
            Task { await viewModel.fetchSelectedFile() }
        }
    }
    
    private func fileNotFoundView(file: OpenFile) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "doc.questionmark")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("\(file.displayName) does not exist")
                .font(.headline)
            
            Text("This file doesn't exist on the server at the expected path.")
                .font(.subheadline)
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
    
    private func editorView(file: OpenFile) -> some View {
        ZStack(alignment: .topTrailing) {
            CodeTextView(text: Binding(
                get: { file.content },
                set: { viewModel.updateContent($0) }
            ))
            
            // Saving indicator
            if viewModel.isSaving {
                HStack(spacing: 8) {
                    ProgressView()
                        .scaleEffect(0.7)
                    Text("Saving...")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                .padding(8)
                .background(Color(nsColor: .controlBackgroundColor).opacity(0.9))
                .cornerRadius(6)
                .padding()
            }
        }
    }
}

#Preview {
    RemoteFileEditorView()
}
