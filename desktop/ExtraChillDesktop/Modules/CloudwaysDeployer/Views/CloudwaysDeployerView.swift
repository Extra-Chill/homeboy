import SwiftUI

struct CloudwaysDeployerView: View {
    @StateObject private var viewModel = CloudwaysDeployerViewModel()
    
    var body: some View {
        VStack(spacing: 0) {
            if !viewModel.hasCredentials || !viewModel.hasSSHKey || !viewModel.hasDeploymentPaths {
                configurationRequiredView
            } else {
                headerSection
                Divider()
                HSplitView {
                    componentListSection
                        .frame(minWidth: 400)
                    consoleSection
                        .frame(minWidth: 300)
                }
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            viewModel.checkConfiguration()
            if viewModel.hasCredentials && viewModel.hasSSHKey && viewModel.hasDeploymentPaths {
                viewModel.refreshVersions()
            }
        }
        .alert("Deploy All Components?", isPresented: $viewModel.showDeployAllConfirmation) {
            Button("Cancel", role: .cancel) { }
            Button("Deploy All", role: .destructive) {
                viewModel.deployAll()
            }
        } message: {
            Text("This will deploy all \(viewModel.components.count) components to production. This may take several minutes.")
        }
    }
    
    // MARK: - Configuration Required View
    
    private var configurationRequiredView: some View {
        VStack(spacing: 20) {
            Image(systemName: "server.rack")
                .font(.system(size: 60))
                .foregroundColor(.secondary)
            
            Text("Cloudways Deployment")
                .font(.title)
            
            Text("Configure your Cloudways server credentials and SSH key in Settings to enable deployment.")
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
            
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: viewModel.hasCredentials ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasCredentials ? .green : .secondary)
                    Text("Server credentials configured")
                }
                HStack {
                    Image(systemName: viewModel.hasSSHKey ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasSSHKey ? .green : .secondary)
                    Text("SSH key configured")
                }
                HStack {
                    Image(systemName: viewModel.hasDeploymentPaths ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.hasDeploymentPaths ? .green : .secondary)
                    Text("Deployment paths configured")
                }
            }
            .padding()
            .background(Color(nsColor: .controlBackgroundColor))
            .cornerRadius(8)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    
    // MARK: - Header Section
    
    private var headerSection: some View {
        HStack(spacing: 16) {
            Button("Deploy Selected (\(viewModel.selectedComponents.count))") {
                viewModel.deploySelected()
            }
            .buttonStyle(.borderedProminent)
            .disabled(viewModel.selectedComponents.isEmpty || viewModel.isDeploying)
            
            Button("Deploy All") {
                viewModel.confirmDeployAll()
            }
            .buttonStyle(.bordered)
            .disabled(viewModel.isDeploying)
            
            Spacer()
            
            if viewModel.isLoading {
                ProgressView()
                    .controlSize(.small)
                Text("Loading...")
                    .foregroundColor(.secondary)
            } else if viewModel.isDeploying {
                ProgressView()
                    .controlSize(.small)
                Text("Deploying...")
                    .foregroundColor(.secondary)
                Button("Cancel") {
                    viewModel.cancelDeployment()
                }
                .buttonStyle(.bordered)
            } else {
                Button {
                    viewModel.refreshVersions()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Refresh versions")
            }
        }
        .padding()
    }
    
    // MARK: - Component List Section
    
    private var componentListSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Column headers
            HStack {
                Text("Component")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 180, alignment: .leading)
                Spacer()
                Text("Local")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 60)
                Text("Remote")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 60)
                Text("Status")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .frame(width: 80)
            }
            .padding(.horizontal)
            
            Divider()
            
            // Component list
            List {
                ForEach(ComponentRegistry.grouped(), id: \.type) { group in
                    Section(header: Text(group.type.rawValue)) {
                        ForEach(group.components) { component in
                            ComponentRow(
                                component: component,
                                isSelected: viewModel.selectedComponents.contains(component.id),
                                localVersion: viewModel.localVersions[component.id],
                                remoteVersion: viewModel.remoteVersions[component.id],
                                status: viewModel.status(for: component),
                                onToggle: { viewModel.toggleSelection(component.id) }
                            )
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            
            Divider()
            
            // Selection controls
            HStack {
                Button("Select Outdated") {
                    viewModel.selectOutdated()
                }
                .buttonStyle(.borderless)
                
                Button("Select All") {
                    viewModel.selectAll()
                }
                .buttonStyle(.borderless)
                
                Button("Deselect All") {
                    viewModel.deselectAll()
                }
                .buttonStyle(.borderless)
                
                Spacer()
                
                Text("\(viewModel.selectedComponents.count) selected")
                    .foregroundColor(.secondary)
                    .font(.caption)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
    }
    
    // MARK: - Console Section
    
    private var consoleSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Console Output")
                    .font(.headline)
                Spacer()
                Button("Clear") {
                    viewModel.consoleOutput = ""
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.isDeploying)
            }
            
            ScrollViewReader { proxy in
                ScrollView {
                    Text(viewModel.consoleOutput.isEmpty ? "Ready to deploy..." : viewModel.consoleOutput)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(viewModel.consoleOutput.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .id("console-bottom")
                }
                .onChange(of: viewModel.consoleOutput) { _, _ in
                    proxy.scrollTo("console-bottom", anchor: .bottom)
                }
            }
            .background(Color(nsColor: .textBackgroundColor))
            .cornerRadius(8)
            
            if let error = viewModel.error {
                Text(error)
                    .foregroundColor(.red)
                    .font(.caption)
            }
        }
        .padding()
    }
}

// MARK: - Component Row

struct ComponentRow: View {
    let component: DeployableComponent
    let isSelected: Bool
    let localVersion: String?
    let remoteVersion: String?
    let status: DeployStatus
    let onToggle: () -> Void
    
    var body: some View {
        HStack {
            // Checkbox
            Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                .foregroundColor(isSelected ? .accentColor : .secondary)
                .onTapGesture { onToggle() }
            
            // Name
            Text(component.name)
                .frame(width: 160, alignment: .leading)
                .lineLimit(1)
            
            Spacer()
            
            // Local version
            Text(localVersion ?? "—")
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(.secondary)
                .frame(width: 60)
            
            // Remote version
            Text(remoteVersion ?? "—")
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(.secondary)
                .frame(width: 60)
            
            // Status
            statusView
                .frame(width: 80)
        }
        .contentShape(Rectangle())
        .onTapGesture { onToggle() }
    }
    
    @ViewBuilder
    private var statusView: some View {
        HStack(spacing: 4) {
            switch status {
            case .current:
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("Current")
                    .font(.caption)
                    .foregroundColor(.green)
            case .needsUpdate:
                Image(systemName: "arrow.up.circle.fill")
                    .foregroundColor(.orange)
                Text("Update")
                    .font(.caption)
                    .foregroundColor(.orange)
            case .missing:
                Image(systemName: "xmark.circle.fill")
                    .foregroundColor(.red)
                Text("Missing")
                    .font(.caption)
                    .foregroundColor(.red)
            case .unknown:
                Image(systemName: "questionmark.circle")
                    .foregroundColor(.gray)
                Text("Unknown")
                    .font(.caption)
                    .foregroundColor(.gray)
            case .deploying:
                ProgressView()
                    .controlSize(.small)
                Text("...")
                    .font(.caption)
                    .foregroundColor(.blue)
            case .failed(let msg):
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.red)
                    .help(msg)
                Text("Failed")
                    .font(.caption)
                    .foregroundColor(.red)
            }
        }
    }
}

#Preview {
    CloudwaysDeployerView()
}
