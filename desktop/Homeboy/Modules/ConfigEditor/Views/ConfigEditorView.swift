import SwiftUI

struct ConfigEditorView: View {
    @StateObject private var viewModel = ConfigEditorViewModel()
    @State private var showCopiedFeedback = false
    
    var body: some View {
        VStack(spacing: 0) {
            tabBar
            Divider()
            headerSection
            Divider()
            toolbarSection
            Divider()
            editorSection
        }
        .frame(minWidth: 600, minHeight: 400)
        .task {
            await viewModel.fetchFile()
        }
        .alert("Unsaved Changes", isPresented: $viewModel.showDiscardChangesAlert) {
            Button("Cancel", role: .cancel) {
                viewModel.cancelDiscardChanges()
            }
            Button("Discard", role: .destructive) {
                viewModel.confirmDiscardChanges()
            }
        } message: {
            Text("You have unsaved changes to \(viewModel.selectedFile.displayName). Discard them?")
        }
        .alert("Confirm Save", isPresented: $viewModel.showSaveConfirmation) {
            Button("Cancel", role: .cancel) {}
            Button("Save") {
                Task { await viewModel.saveFile() }
            }
        } message: {
            Text("This will overwrite \(viewModel.selectedFile.displayName) on the server. The current server version will be backed up locally.\n\n\(viewModel.selectedFile.saveWarning)")
        }
        .alert("Restore from Backup", isPresented: $viewModel.showRollbackConfirmation) {
            Button("Cancel", role: .cancel) {
                viewModel.selectedBackup = nil
            }
            Button("Restore") {
                if let backup = viewModel.selectedBackup {
                    viewModel.restoreBackup(backup)
                    viewModel.selectedBackup = nil
                }
            }
        } message: {
            if let backup = viewModel.selectedBackup {
                Text("Load the version from \(backup.displayDate) into the editor? You can review the content before saving to the server.")
            } else {
                Text("Restore this backup?")
            }
        }
        .alert("Create File", isPresented: $viewModel.showCreateFileConfirmation) {
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                Task { await viewModel.createFile() }
            }
        } message: {
            Text("Create \(viewModel.selectedFile.displayName) with default configuration?")
        }
    }
    
    // MARK: - Tab Bar
    
    private var tabBar: some View {
        HStack(spacing: 0) {
            ForEach(ConfigFile.allCases) { file in
                tabButton(for: file)
            }
            Spacer()
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    private func tabButton(for file: ConfigFile) -> some View {
        Button {
            _ = viewModel.selectFile(file)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: file.icon)
                    .font(.caption)
                Text(file.displayName)
                    .font(.subheadline)
                
                // Unsaved indicator for selected file
                if file == viewModel.selectedFile && viewModel.hasUnsavedChanges {
                    Circle()
                        .fill(Color.orange)
                        .frame(width: 6, height: 6)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(file == viewModel.selectedFile ? Color.accentColor.opacity(0.2) : Color.clear)
            )
            .foregroundColor(file == viewModel.selectedFile ? .accentColor : .secondary)
        }
        .buttonStyle(.plain)
    }
    
    // MARK: - Header
    
    private var headerSection: some View {
        HStack {
            Image(systemName: viewModel.selectedFile.icon)
                .foregroundColor(.accentColor)
            Text(viewModel.selectedFile.displayName)
                .font(.headline)
            
            Spacer()
            
            if viewModel.hasUnsavedChanges {
                Text("Modified")
                    .font(.caption)
                    .foregroundColor(.orange)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Color.orange.opacity(0.1))
                    .cornerRadius(4)
            }
            
            Text("Fetched: \(viewModel.lastFetchedFormatted)")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding()
    }
    
    // MARK: - Toolbar
    
    private var toolbarSection: some View {
        HStack(spacing: 12) {
            // Refresh
            Button {
                Task { await viewModel.fetchFile() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(viewModel.isLoading || viewModel.isSaving)
            
            // Rollback Menu
            Menu {
                if viewModel.backups.isEmpty {
                    Text("No backups yet")
                } else {
                    ForEach(viewModel.backups) { backup in
                        Button(backup.displayDate) {
                            viewModel.selectedBackup = backup
                            viewModel.showRollbackConfirmation = true
                        }
                    }
                    
                    Divider()
                    
                    Button("Clear History", role: .destructive) {
                        viewModel.clearBackups()
                    }
                }
            } label: {
                Label("Rollback", systemImage: "clock.arrow.circlepath")
            }
            .disabled(viewModel.backups.isEmpty || viewModel.isLoading || viewModel.isSaving)
            
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
            .disabled(viewModel.content.isEmpty)
            .help("Copy to clipboard")
            
            // Save
            Button {
                viewModel.showSaveConfirmation = true
            } label: {
                HStack(spacing: 4) {
                    Label("Save", systemImage: "square.and.arrow.up")
                    if viewModel.hasUnsavedChanges {
                        Circle()
                            .fill(Color.orange)
                            .frame(width: 6, height: 6)
                    }
                }
            }
            .disabled(!viewModel.hasUnsavedChanges || viewModel.isLoading || viewModel.isSaving)
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Editor Section
    
    private var editorSection: some View {
        Group {
            if viewModel.isLoading && viewModel.content.isEmpty {
                loadingView
            } else if let error = viewModel.error {
                errorView(error)
            } else if !viewModel.fileExists {
                fileNotFoundView
            } else {
                editorView
            }
        }
    }
    
    private var loadingView: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading \(viewModel.selectedFile.displayName)...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func errorView(_ error: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.largeTitle)
                .foregroundColor(.orange)
            Text("Error")
                .font(.headline)
            Text(error)
                .font(.caption)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
            
            Button("Retry") {
                Task { await viewModel.fetchFile() }
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
    
    private var fileNotFoundView: some View {
        VStack(spacing: 16) {
            Image(systemName: "doc.questionmark")
                .font(.system(size: 48))
                .foregroundColor(.secondary)
            
            Text("\(viewModel.selectedFile.displayName) does not exist")
                .font(.headline)
            
            Text("This file doesn't exist on the server yet.")
                .font(.subheadline)
                .foregroundColor(.secondary)
            
            if viewModel.selectedFile.canCreate {
                Button {
                    viewModel.showCreateFileConfirmation = true
                } label: {
                    Label("Create File", systemImage: "plus.circle")
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.isSaving)
            } else {
                Text("This file should exist on any WordPress installation.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
    
    private var editorView: some View {
        ZStack(alignment: .topTrailing) {
            CodeTextView(text: $viewModel.content)
            
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
    ConfigEditorView()
}
