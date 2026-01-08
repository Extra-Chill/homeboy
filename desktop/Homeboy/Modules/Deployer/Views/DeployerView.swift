import SwiftUI

struct DeployerView: View {
    @StateObject private var viewModel = DeployerViewModel()
    @State private var showCopiedFeedback = false
    
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
            
            Text("Deployment")
                .font(.title)
            
            Text("Configure your server credentials and SSH key in Settings to enable deployment.")
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)
            
            VStack(alignment: .leading, spacing: 8) {
                if let serverName = viewModel.serverName {
                    Text("Server: \(serverName)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                } else {
                    Text("No server linked to project")
                        .font(.caption)
                        .foregroundColor(.orange)
                }
                
                Divider()
                
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
                    Text("Remote base path configured")
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
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if !viewModel.themes.isEmpty {
                        componentTable(title: "Themes", components: viewModel.themes)
                    }
                    if !viewModel.networkPlugins.isEmpty {
                        componentTable(title: "Network Plugins", components: viewModel.networkPlugins)
                    }
                    if !viewModel.sitePlugins.isEmpty {
                        componentTable(title: "Site Plugins", components: viewModel.sitePlugins)
                    }
                }
                .padding(.vertical, 8)
            }
            
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
    
    private func componentTable(title: String, components: [DeployableComponent]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title.uppercased())
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.secondary)
                .padding(.horizontal, 8)
            
            Table(components, selection: $viewModel.selectedComponents, sortOrder: $viewModel.sortOrder) {
                TableColumn("Component", value: \.name)
                    .width(min: 140, ideal: 180)
                
                TableColumn("Local") { component in
                    Text(viewModel.localVersions[component.id] ?? "—")
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.secondary)
                }
                .width(60)
                
                TableColumn("Remote") { component in
                    Text(viewModel.remoteVersions[component.id] ?? "—")
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.secondary)
                }
                .width(60)
                
                TableColumn("Status") { component in
                    statusView(for: viewModel.status(for: component))
                }
                .width(min: 70, ideal: 90)
            }
            .frame(height: CGFloat(components.count * 24 + 28))
        }
    }
    
    @ViewBuilder
    private func statusView(for status: DeployStatus) -> some View {
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
    
    // MARK: - Console Section
    
    private var consoleSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Console Output")
                    .font(.headline)
                Spacer()
                Button {
                    viewModel.copyConsoleOutput()
                    showCopiedFeedback = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                        showCopiedFeedback = false
                    }
                } label: {
                    Image(systemName: showCopiedFeedback ? "checkmark" : "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.consoleOutput.isEmpty)
                .help("Copy output")
                Button {
                    viewModel.consoleOutput = ""
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.isDeploying)
                .help("Clear output")
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

#Preview {
    DeployerView()
}
