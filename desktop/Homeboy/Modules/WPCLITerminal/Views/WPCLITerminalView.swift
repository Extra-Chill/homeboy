import SwiftUI

struct WPCLITerminalView: View {
    @StateObject private var viewModel = WPCLITerminalViewModel()
    @State private var showCopiedFeedback = false
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Image(systemName: "terminal")
                    .foregroundColor(.accentColor)
                Text("WP-CLI Terminal")
                    .font(.headline)
                Spacer()
                
                // Environment-aware path display
                Group {
                    if viewModel.environment == .local {
                        Text(viewModel.localWPPath)
                    } else {
                        HStack(spacing: 4) {
                            Image(systemName: "server.rack")
                            Text(viewModel.productionDomain)
                        }
                    }
                }
                .font(.caption)
                .foregroundColor(viewModel.environment == .local ? .secondary : .orange)
                .lineLimit(1)
                .truncationMode(.middle)
            }
            .padding()
            
            Divider()
            
            // Toolbar: Environment selector, site selector, actions
            HStack {
                // Environment selector
                Picker("", selection: $viewModel.environment) {
                    ForEach(TerminalEnvironment.allCases, id: \.self) { env in
                        Text(env.rawValue).tag(env)
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 160)
                .help(viewModel.isProductionConfigured ? "Switch between local and production" : "Configure SSH in Settings to enable production mode")
                
                // Site selector (show for multisite in either environment)
                if viewModel.isMultisite || (viewModel.environment == .production && viewModel.hasProductionMultisite) {
                    Text("Site:")
                        .foregroundColor(.secondary)
                    Picker("", selection: Binding(
                        get: { viewModel.selectedSite.id },
                        set: { id in
                            if let site = viewModel.networkSites.first(where: { $0.id == id }) {
                                viewModel.selectSite(site)
                            }
                        }
                    )) {
                        ForEach(viewModel.networkSites) { site in
                            Text("\(site.name) (\(site.blogId))").tag(site.id)
                        }
                    }
                    .frame(width: 180)
                }
                
                Spacer()
                
                Button {
                    viewModel.copyOutput()
                    showCopiedFeedback = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                        showCopiedFeedback = false
                    }
                } label: {
                    Image(systemName: showCopiedFeedback ? "checkmark" : "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .disabled(viewModel.output.isEmpty)
                .help("Copy output")
                
                Button {
                    viewModel.clearOutput()
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Clear output")
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            
            Divider()
            
            // Terminal output
            ScrollViewReader { proxy in
                ScrollView {
                    Text(viewModel.output.isEmpty ? "$ " : viewModel.output)
                        .font(.system(.body, design: .monospaced))
                        .foregroundColor(viewModel.output.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding()
                        .id("terminal-bottom")
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: viewModel.output) { _, _ in
                    proxy.scrollTo("terminal-bottom", anchor: .bottom)
                }
            }
            
            Divider()
            
            // Command input
            HStack(spacing: 12) {
                Text("$")
                    .font(.system(.body, design: .monospaced))
                    .foregroundColor(.secondary)
                
                TextField("Enter WP-CLI command...", text: $viewModel.command)
                    .textFieldStyle(.plain)
                    .font(.system(.body, design: .monospaced))
                    .onSubmit {
                        viewModel.runCommand()
                    }
                    .onKeyPress(.upArrow) {
                        viewModel.navigateHistory(direction: -1)
                        return .handled
                    }
                    .onKeyPress(.downArrow) {
                        viewModel.navigateHistory(direction: 1)
                        return .handled
                    }
                    .disabled(viewModel.isRunning)
                
                if viewModel.isRunning {
                    ProgressView()
                        .controlSize(.small)
                    
                    Button("Cancel") {
                        viewModel.cancelCommand()
                    }
                    .buttonStyle(.bordered)
                } else {
                    Button("Run") {
                        viewModel.runCommand()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(viewModel.command.isEmpty)
                    .keyboardShortcut(.return, modifiers: [])
                }
            }
            .padding()
        }
        .frame(minWidth: 600, minHeight: 400)
    }
}

#Preview {
    WPCLITerminalView()
}
