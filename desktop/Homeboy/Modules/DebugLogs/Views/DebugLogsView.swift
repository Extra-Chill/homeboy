import SwiftUI

struct DebugLogsView: View {
    @StateObject private var viewModel = DebugLogsViewModel()
    @State private var showCopiedFeedback = false
    
    var body: some View {
        VStack(spacing: 0) {
            headerSection
            Divider()
            toolbarSection
            Divider()
            searchSection
            Divider()
            logContentSection
        }
        .frame(minWidth: 600, minHeight: 400)
        .task {
            await viewModel.fetchLogs()
        }
    }
    
    // MARK: - Header
    
    private var headerSection: some View {
        HStack {
            Image(systemName: "doc.text.magnifyingglass")
                .foregroundColor(.accentColor)
            Text("Debug Logs")
                .font(.headline)
            
            Spacer()
            
            if viewModel.fileSize > 0 {
                Text(viewModel.fileSizeFormatted)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Color.secondary.opacity(0.1))
                    .cornerRadius(4)
            }
            
            Text("Updated: \(viewModel.lastUpdatedFormatted)")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding()
    }
    
    // MARK: - Toolbar
    
    private var toolbarSection: some View {
        HStack(spacing: 12) {
            Button {
                Task { await viewModel.fetchLogs() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(viewModel.isLoading)
            
            Picker("Lines", selection: $viewModel.selectedLineCount) {
                ForEach(LineCount.allCases) { count in
                    Text(count.displayName).tag(count)
                }
            }
            .frame(width: 100)
            .onChange(of: viewModel.selectedLineCount) { _, _ in
                Task { await viewModel.fetchLogs() }
            }
            
            Spacer()
            
            Button {
                viewModel.copyLogs()
                showCopiedFeedback = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    showCopiedFeedback = false
                }
            } label: {
                Image(systemName: showCopiedFeedback ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(.borderless)
            .disabled(viewModel.logContent.isEmpty)
            .help("Copy logs to clipboard")
            
            Button {
                Task { await viewModel.clearLogs() }
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .disabled(viewModel.isLoading)
            .help("Delete debug.log file")
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }
    
    // MARK: - Search
    
    private var searchSection: some View {
        HStack {
            Image(systemName: "magnifyingglass")
                .foregroundColor(.secondary)
            TextField("Filter logs...", text: $viewModel.searchText)
                .textFieldStyle(.plain)
            
            if !viewModel.searchText.isEmpty {
                Button {
                    viewModel.searchText = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    // MARK: - Log Content
    
    private var logContentSection: some View {
        Group {
            if viewModel.isLoading && viewModel.logContent.isEmpty {
                loadingView
            } else if let error = viewModel.error {
                errorView(error)
            } else if viewModel.filteredContent.isEmpty {
                emptyView
            } else {
                logScrollView
            }
        }
    }
    
    private var loadingView: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading debug.log...")
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private func errorView(_ error: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.largeTitle)
                .foregroundColor(.orange)
            Text("Error loading logs")
                .font(.headline)
            Text(error)
                .font(.caption)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
            
            Button("Retry") {
                Task { await viewModel.fetchLogs() }
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
    
    private var emptyView: some View {
        VStack(spacing: 12) {
            Image(systemName: "doc.text")
                .font(.largeTitle)
                .foregroundColor(.secondary)
            
            if viewModel.searchText.isEmpty {
                Text("No debug.log found or file is empty")
                    .foregroundColor(.secondary)
            } else {
                Text("No matches for \"\(viewModel.searchText)\"")
                    .foregroundColor(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    private var logScrollView: some View {
        ScrollViewReader { proxy in
            ScrollView {
                Text(viewModel.filteredContent)
                    .font(.system(.body, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding()
                    .id("log-bottom")
            }
            .background(Color(nsColor: .textBackgroundColor))
            .onChange(of: viewModel.logContent) { _, _ in
                proxy.scrollTo("log-bottom", anchor: .bottom)
            }
        }
    }
}

#Preview {
    DebugLogsView()
}
