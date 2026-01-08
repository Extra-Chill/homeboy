import SwiftUI

/// Main container view for the Database Browser module
struct DatabaseBrowserView: View {
    @StateObject private var viewModel = DatabaseBrowserViewModel()
    
    var body: some View {
        VStack(spacing: 0) {
            // Top toolbar
            toolbar
            
            Divider()
            
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
        .alert("Error", isPresented: .constant(viewModel.errorMessage != nil)) {
            Button("OK") {
                viewModel.errorMessage = nil
            }
            Button("Copy Error") {
                viewModel.errorMessage?.copyToClipboard()
                viewModel.errorMessage = nil
            }
        } message: {
            if let error = viewModel.errorMessage {
                Text(error.body)
            }
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
                            Text("This is a protected WordPress core table")
                        }
                        .foregroundColor(.red)
                        .font(.headline)
                        
                        Text("Dropping this table would break your WordPress installation. This action is not allowed.")
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
            
            if viewModel.connectionStatus.isConnected {
                Text("\(viewModel.totalTableCount) tables")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            // Connect/Disconnect button
            if viewModel.connectionStatus.isConnected {
                Button("Disconnect") {
                    viewModel.disconnect()
                }
            } else {
                Button("Connect") {
                    Task {
                        await viewModel.connect()
                    }
                }
                .disabled(!viewModel.isConfigured || viewModel.connectionStatus == .connecting)
            }
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
            
            if !viewModel.isConfigured {
                VStack(spacing: 8) {
                    Text("Database credentials not configured")
                        .foregroundColor(.secondary)
                    
                    Text("Go to Settings to configure your remote database credentials and SSH key.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)
                }
            } else {
                VStack(spacing: 8) {
                    Text("Not connected")
                        .foregroundColor(.secondary)
                    
                    Text("Click Connect to browse your remote database.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
            if case .error(let message) = viewModel.connectionStatus {
                InlineErrorView(message, source: "Database Browser")
                    .frame(maxWidth: 400)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    DatabaseBrowserView()
}
