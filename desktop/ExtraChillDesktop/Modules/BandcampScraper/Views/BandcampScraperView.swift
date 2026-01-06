import SwiftUI

struct BandcampScraperView: View {
    @StateObject private var viewModel = BandcampScraperViewModel()
    @State private var showingSetup = false
    @State private var setupOutput = ""
    @State private var isSettingUp = false
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            headerSection
            
            Divider()
            
            // Main content
            HSplitView {
                // Left: Console output
                consoleSection
                    .frame(minWidth: 300)
                
                // Right: Results table
                resultsSection
                    .frame(minWidth: 400)
            }
        }
        .frame(minWidth: 800, minHeight: 600)
        .onAppear {
            if !viewModel.checkPythonSetup() {
                showingSetup = true
            }
        }
        .sheet(isPresented: $showingSetup) {
            setupSheet
        }
    }
    
    // MARK: - Header Section
    
    private var headerSection: some View {
        HStack(spacing: 16) {
            // Tag input
            VStack(alignment: .leading, spacing: 4) {
                Text("Tag")
                    .font(.caption)
                    .foregroundColor(.secondary)
                TextField("e.g., south-carolina, lo-fi", text: $viewModel.tag)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 200)
                    .disabled(viewModel.isRunning)
            }
            
            // Clicks stepper
            VStack(alignment: .leading, spacing: 4) {
                Text("View More Clicks")
                    .font(.caption)
                    .foregroundColor(.secondary)
                Stepper("\(viewModel.clicks)", value: $viewModel.clicks, in: 1...10)
                    .frame(width: 100)
                    .disabled(viewModel.isRunning)
            }
            
            Spacer()
            
            // Action buttons
            if viewModel.isRunning {
                Button("Cancel") {
                    viewModel.cancelScrape()
                }
                .buttonStyle(.bordered)
                
                ProgressView()
                    .controlSize(.small)
            } else {
                Button("Start Scrape") {
                    viewModel.startScrape()
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.tag.isEmpty)
            }
        }
        .padding()
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
                .disabled(viewModel.isRunning)
            }
            
            ScrollViewReader { proxy in
                ScrollView {
                    Text(viewModel.consoleOutput.isEmpty ? "Ready to scrape..." : viewModel.consoleOutput)
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
    
    // MARK: - Results Section
    
    private var resultsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Results")
                    .font(.headline)
                Text("(\(viewModel.results.count) emails found)")
                    .foregroundColor(.secondary)
                
                Spacer()
                
                // Sendy List ID
                TextField("Sendy List ID", text: $viewModel.sendyListId)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 150)
                
                Button("Add Selected") {
                    Task {
                        await viewModel.subscribeToNewsletter()
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.selectedEmails.isEmpty || viewModel.sendyListId.isEmpty || viewModel.isSubscribing)
            }
            
            if let result = viewModel.subscribeResult {
                Text(result)
                    .font(.caption)
                    .foregroundColor(result.contains("failed") ? .red : .green)
            }
            
            // Results table
            Table(viewModel.results, selection: $viewModel.selectedEmails) {
                TableColumn("") { email in
                    Image(systemName: viewModel.selectedEmails.contains(email.email) ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(viewModel.selectedEmails.contains(email.email) ? .accentColor : .secondary)
                        .onTapGesture {
                            viewModel.toggleSelection(email.email)
                        }
                }
                .width(30)
                
                TableColumn("Email", value: \.email)
                    .width(min: 150, ideal: 200)
                
                TableColumn("Artist", value: \.name)
                    .width(min: 100, ideal: 150)
                
                TableColumn("Notes") { email in
                    Text(email.notes)
                        .lineLimit(2)
                        .help(email.notes)
                }
                .width(min: 150)
            }
            
            // Selection controls
            HStack {
                Button("Select All") {
                    viewModel.selectAll()
                }
                .buttonStyle(.borderless)
                
                Button("Deselect All") {
                    viewModel.deselectAll()
                }
                .buttonStyle(.borderless)
                
                Spacer()
                
                Text("\(viewModel.selectedEmails.count) selected")
                    .foregroundColor(.secondary)
                    .font(.caption)
            }
        }
        .padding()
    }
    
    // MARK: - Setup Sheet
    
    private var setupSheet: some View {
        VStack(spacing: 20) {
            Image(systemName: "gear.badge.questionmark")
                .font(.system(size: 50))
                .foregroundColor(.orange)
            
            Text("Python Setup Required")
                .font(.title)
            
            Text("The Bandcamp Scraper needs to set up a Python environment with Playwright.")
                .multilineTextAlignment(.center)
                .foregroundColor(.secondary)
            
            if isSettingUp {
                ScrollView {
                    Text(setupOutput)
                        .font(.system(.caption, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(height: 200)
                .background(Color(nsColor: .textBackgroundColor))
                .cornerRadius(8)
                
                ProgressView()
            } else {
                Button("Set Up Python Environment") {
                    runSetup()
                }
                .buttonStyle(.borderedProminent)
            }
            
            Button("Cancel") {
                showingSetup = false
            }
            .buttonStyle(.bordered)
            .disabled(isSettingUp)
        }
        .padding(40)
        .frame(width: 500)
    }
    
    private func runSetup() {
        isSettingUp = true
        setupOutput = "Creating virtual environment...\n"
        
        viewModel.setupPython(
            onOutput: { line in
                setupOutput += line
            },
            onComplete: { result in
                isSettingUp = false
                switch result {
                case .success:
                    setupOutput += "\n✓ Setup complete!"
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                        showingSetup = false
                    }
                case .failure(let error):
                    setupOutput += "\n✗ Setup failed: \(error.localizedDescription)"
                }
            }
        )
    }
}

#Preview {
    BandcampScraperView()
}
