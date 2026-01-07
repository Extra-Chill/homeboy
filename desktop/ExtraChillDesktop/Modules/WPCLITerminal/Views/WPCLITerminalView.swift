import SwiftUI

struct WPCLITerminalView: View {
    @StateObject private var terminalViewModel = WPCLITerminalViewModel()
    @StateObject private var scraperViewModel = ScraperTesterViewModel()
    @State private var selectedTab = 0
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Image(systemName: "terminal")
                    .foregroundColor(.accentColor)
                Text("WP-CLI Terminal")
                    .font(.headline)
                Spacer()
                Text(terminalViewModel.localWPPath)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .padding()
            
            // Tab picker
            Picker("", selection: $selectedTab) {
                Text("Terminal").tag(0)
                Text("Scraper Tester").tag(1)
            }
            .pickerStyle(.segmented)
            .padding(.horizontal)
            .padding(.bottom, 8)
            
            Divider()
            
            if selectedTab == 0 {
                terminalView
            } else {
                scraperTesterView
            }
        }
        .frame(minWidth: 600, minHeight: 400)
    }
    
    // MARK: - Terminal Tab
    
    private var terminalView: some View {
        VStack(spacing: 0) {
            // Site selector
            HStack {
                Text("Site:")
                    .foregroundColor(.secondary)
                Picker("", selection: Binding(
                    get: { terminalViewModel.selectedSite.id },
                    set: { id in
                        if let site = WPCLITerminalViewModel.networkSites.first(where: { $0.id == id }) {
                            terminalViewModel.selectSite(site)
                        }
                    }
                )) {
                    ForEach(WPCLITerminalViewModel.networkSites) { site in
                        Text("\(site.name) (\(site.blogId))").tag(site.id)
                    }
                }
                .frame(width: 180)
                
                Spacer()
                
                Button {
                    terminalViewModel.clearOutput()
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
                    Text(terminalViewModel.output.isEmpty ? "$ " : terminalViewModel.output)
                        .font(.system(.body, design: .monospaced))
                        .foregroundColor(terminalViewModel.output.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding()
                        .id("terminal-bottom")
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: terminalViewModel.output) { _, _ in
                    proxy.scrollTo("terminal-bottom", anchor: .bottom)
                }
            }
            
            Divider()
            
            // Command input
            HStack(spacing: 12) {
                Text("$")
                    .font(.system(.body, design: .monospaced))
                    .foregroundColor(.secondary)
                
                TextField("Enter WP-CLI command...", text: $terminalViewModel.command)
                    .textFieldStyle(.plain)
                    .font(.system(.body, design: .monospaced))
                    .onSubmit {
                        terminalViewModel.runCommand()
                    }
                    .onKeyPress(.upArrow) {
                        terminalViewModel.navigateHistory(direction: -1)
                        return .handled
                    }
                    .onKeyPress(.downArrow) {
                        terminalViewModel.navigateHistory(direction: 1)
                        return .handled
                    }
                    .disabled(terminalViewModel.isRunning)
                
                if terminalViewModel.isRunning {
                    ProgressView()
                        .controlSize(.small)
                    
                    Button("Cancel") {
                        terminalViewModel.cancelCommand()
                    }
                    .buttonStyle(.bordered)
                } else {
                    Button("Run") {
                        terminalViewModel.runCommand()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(terminalViewModel.command.isEmpty)
                    .keyboardShortcut(.return, modifiers: [])
                }
            }
            .padding()
        }
    }
    
    // MARK: - Scraper Tester Tab
    
    private var scraperTesterView: some View {
        VStack(spacing: 0) {
            // Input form
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Target URL")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    TextField("https://venue.com/events", text: $scraperViewModel.targetUrl)
                        .textFieldStyle(.roundedBorder)
                }
                
                VStack(alignment: .leading, spacing: 4) {
                    Text("Venue Name Override (optional)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    TextField("e.g., The Music Hall", text: $scraperViewModel.venueName)
                        .textFieldStyle(.roundedBorder)
                }
                
                HStack {
                    Stepper("Max Results: \(scraperViewModel.maxResults)", value: $scraperViewModel.maxResults, in: 1...10)
                    
                    Spacer()
                    
                    Toggle("Upsert Events", isOn: $scraperViewModel.doUpsert)
                }
                
                if scraperViewModel.doUpsert {
                    Text("Warning: This will create/update events in the database")
                        .font(.caption)
                        .foregroundColor(.orange)
                }
            }
            .padding()
            
            // Action buttons
            HStack {
                Button(scraperViewModel.isRunning ? "Running..." : "Run Test") {
                    scraperViewModel.runTest()
                }
                .buttonStyle(.borderedProminent)
                .disabled(scraperViewModel.targetUrl.isEmpty || scraperViewModel.isRunning)
                
                if scraperViewModel.isRunning {
                    ProgressView()
                        .controlSize(.small)
                    
                    Button("Cancel") {
                        scraperViewModel.cancelTest()
                    }
                    .buttonStyle(.bordered)
                }
                
                Spacer()
                
                Button("Copy Output") {
                    scraperViewModel.copyOutput()
                }
                .disabled(scraperViewModel.output.isEmpty)
                
                Button {
                    scraperViewModel.clearOutput()
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Clear output")
            }
            .padding(.horizontal)
            .padding(.bottom, 8)
            
            Divider()
            
            // Output area
            ScrollViewReader { proxy in
                ScrollView {
                    Text(scraperViewModel.output.isEmpty ? "Output will appear here..." : scraperViewModel.output)
                        .font(.system(.body, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .foregroundColor(scraperViewModel.output.isEmpty ? .secondary : .primary)
                        .textSelection(.enabled)
                        .padding()
                        .id("scraperOutput")
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: scraperViewModel.output) { _, _ in
                    proxy.scrollTo("scraperOutput", anchor: .bottom)
                }
            }
        }
    }
}

#Preview {
    WPCLITerminalView()
}
