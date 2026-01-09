import SwiftUI

/// Main container view for the Database Browser module
struct DatabaseBrowserView: View {
    @StateObject private var viewModel = DatabaseBrowserViewModel()
    
    var body: some View {
        VStack(spacing: 0) {
            // Top toolbar (only shown when connected)
            if viewModel.connectionStatus.isConnected {
                toolbar
                Divider()
            }
            
            // Main content
            if viewModel.connectionStatus.isConnected {
                HSplitView {
                    // Site list sidebar
                    SiteListView(viewModel: viewModel)
                        .frame(minWidth: 220, idealWidth: 280, maxWidth: 350)
                    
                    // Table data view
                    TableDataView(viewModel: viewModel)
                }
            } else {
                disconnectedView
            }
        }
        .onAppear {
            Task { await viewModel.connectIfConfigured() }
        }
        .sheet(isPresented: $viewModel.showRowDeletionConfirm) {
            rowDeletionConfirmSheet
        }
        .sheet(isPresented: $viewModel.showTableDeletionConfirm) {
            tableDeletionConfirmSheet
        }
    }
    
    // MARK: - Row Deletion Confirmation
    
    private var rowDeletionConfirmSheet: some View {
        VStack(spacing: 20) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 48))
                .foregroundColor(.orange)
            
            Text("Delete Row?")
                .font(.title2)
                .fontWeight(.semibold)
            
            if let pending = viewModel.pendingRowDeletion {
                VStack(spacing: 8) {
                    Text("Table: \(pending.table)")
                        .font(.headline)
                    Text("\(pending.primaryKeyColumn) = \(pending.primaryKeyValue)")
                        .font(.system(.body, design: .monospaced))
                        .foregroundColor(.secondary)
                    if !pending.rowPreview.isEmpty {
                        Text(pending.rowPreview)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                    }
                }
                .padding()
                .background(Color(nsColor: .controlBackgroundColor))
                .cornerRadius(8)
            }
            
            Text("Type **delete** to confirm")
                .font(.callout)
            
            TextField("", text: $viewModel.rowDeletionConfirmText)
                .textFieldStyle(.roundedBorder)
                .frame(width: 200)
                .multilineTextAlignment(.center)
            
            HStack(spacing: 16) {
                Button("Cancel") {
                    viewModel.cancelRowDeletion()
                }
                .keyboardShortcut(.escape)
                
                Button("Delete Row") {
                    Task { await viewModel.confirmRowDeletion() }
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .disabled(!viewModel.canConfirmRowDeletion || viewModel.isDeletingRow)
            }
        }
        .padding(32)
        .frame(width: 400)
    }
    
    // MARK: - Table Deletion Confirmation
    
    private var tableDeletionConfirmSheet: some View {
        VStack(spacing: 20) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 48))
                .foregroundColor(.red)
            
            Text("Drop Table?")
                .font(.title2)
                .fontWeight(.semibold)
            
            if let pending = viewModel.pendingTableDeletion {
                if pending.isProtected {
                    // Protected table warning
                    VStack(spacing: 12) {
                        HStack {
                            Image(systemName: "lock.fill")
                            Text("This is a protected core table")
                        }
                        .foregroundColor(.red)
                        .font(.headline)
                        
                        Text("Dropping this table would break your installation. This action is not allowed.")
                            .foregroundColor(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .padding()
                    .background(Color.red.opacity(0.1))
                    .cornerRadius(8)
                    
                    Button("OK") {
                        viewModel.cancelTableDeletion()
                    }
                    .buttonStyle(.borderedProminent)
                } else {
                    // Normal table - allow deletion
                    VStack(spacing: 8) {
                        Text(pending.table.name)
                            .font(.system(.headline, design: .monospaced))
                        
                        HStack(spacing: 16) {
                            Text("\(pending.table.rowCount) rows")
                            Text(pending.table.formattedSize)
                        }
                        .font(.caption)
                        .foregroundColor(.secondary)
                    }
                    .padding()
                    .background(Color(nsColor: .controlBackgroundColor))
                    .cornerRadius(8)
                    
                    Text("This action **cannot be undone**. All data will be permanently deleted.")
                        .font(.callout)
                        .foregroundColor(.red)
                        .multilineTextAlignment(.center)
                    
                    Text("Type the table name to confirm:")
                        .font(.callout)
                    
                    TextField("", text: $viewModel.tableDeletionConfirmText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 300)
                        .font(.system(.body, design: .monospaced))
                        .multilineTextAlignment(.center)
                    
                    HStack(spacing: 16) {
                        Button("Cancel") {
                            viewModel.cancelTableDeletion()
                        }
                        .keyboardShortcut(.escape)
                        
                        Button("Drop Table") {
                            Task { await viewModel.confirmTableDeletion() }
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(.red)
                        .disabled(!viewModel.canConfirmTableDeletion || viewModel.isDeletingTable)
                    }
                }
            }
        }
        .padding(32)
        .frame(width: 450)
    }
    
    // MARK: - Toolbar
    
    private var toolbar: some View {
        HStack(spacing: 16) {
            // Connection status indicator
            HStack(spacing: 6) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 8, height: 8)
                
                Text(viewModel.connectionStatus.statusText)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Text("\(viewModel.totalTableCount) tables")
                .font(.caption)
                .foregroundColor(.secondary)
            
            Spacer()
        }
        .padding()
    }
    
    private var statusColor: Color {
        switch viewModel.connectionStatus {
        case .disconnected:
            return .gray
        case .connecting:
            return .orange
        case .connected:
            return .green
        case .error:
            return .red
        }
    }
    
    // MARK: - Disconnected View
    
    private var disconnectedView: some View {
        VStack(spacing: 20) {
            Image(systemName: "cylinder.split.1x2")
                .font(.system(size: 64))
                .foregroundColor(.secondary)
            
            Text("Database Browser")
                .font(.title2)
            
            // Show appropriate message based on state
            switch viewModel.connectionStatus {
            case .connecting:
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Connecting...")
                        .foregroundColor(.secondary)
                }
                
            case .error(let message):
                VStack(spacing: 12) {
                    ErrorView(
                        AppError(message, source: "Database Browser"),
                        onRetry: {
                            Task { await viewModel.retry() }
                        }
                    )
                    .frame(maxWidth: 500)
                }
                
            case .disconnected:
                if !viewModel.isConfigured {
                    VStack(spacing: 8) {
                        Text("Database not configured")
                            .foregroundColor(.secondary)
                        
                        Text("Configure your database credentials in Settings → Database to browse your remote database.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: 400)
                    }
                } else {
                    // Configured but disconnected - should auto-connect, show spinner
                    HStack(spacing: 8) {
                        ProgressView()
                            .controlSize(.small)
                        Text("Connecting...")
                            .foregroundColor(.secondary)
                    }
                }
                
            case .connected:
                // Shouldn't reach here, but handle gracefully
                EmptyView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    DatabaseBrowserView()
}
