import SwiftUI

/// View shown when a module needs dependency installation
struct ModuleSetupView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    var body: some View {
        VStack(spacing: 20) {
            if viewModel.isSettingUp {
                // Setup in progress
                VStack(spacing: 16) {
                    ProgressView()
                        .scaleEffect(1.5)
                    
                    Text("Setting up \(module.name)...")
                        .font(.headline)
                    
                    Text("Installing Python dependencies and Playwright browsers")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                .padding(.top, 40)
                
                // Console output during setup
                ModuleConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                    .frame(maxHeight: 300)
                    .padding(.horizontal)
                
            } else {
                // Setup required prompt
                VStack(spacing: 16) {
                    Image(systemName: "wrench.and.screwdriver")
                        .font(.system(size: 48))
                        .foregroundColor(.orange)
                    
                    Text("Setup Required")
                        .font(.headline)
                    
                    Text("This module requires Python dependencies to be installed before it can run.")
                        .font(.body)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 400)
                    
                    // Dependencies list
                    if let dependencies = module.manifest.runtime.dependencies, !dependencies.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Dependencies:")
                                .font(.caption)
                                .foregroundColor(.secondary)
                            
                            Text(dependencies.joined(separator: ", "))
                                .font(.system(.caption, design: .monospaced))
                        }
                        .padding()
                        .background(Color(nsColor: .controlBackgroundColor))
                        .cornerRadius(8)
                    }
                    
                    if let browsers = module.manifest.runtime.playwrightBrowsers, !browsers.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Playwright Browsers:")
                                .font(.caption)
                                .foregroundColor(.secondary)
                            
                            Text(browsers.joined(separator: ", "))
                                .font(.system(.caption, design: .monospaced))
                        }
                        .padding()
                        .background(Color(nsColor: .controlBackgroundColor))
                        .cornerRadius(8)
                    }
                    
                    Button {
                        viewModel.setup(module: module)
                    } label: {
                        HStack {
                            Image(systemName: "arrow.down.circle")
                            Text("Install Dependencies")
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                }
                .padding(40)
                
                if let error = viewModel.error {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(error)
                            .font(.caption)
                            .foregroundColor(.red)
                        
                        // Show console output if there was an error (so user can see what went wrong)
                        if !viewModel.consoleOutput.isEmpty {
                            Text("Console Output:")
                                .font(.caption)
                                .foregroundColor(.secondary)
                            
                            ModuleConsoleView(output: $viewModel.consoleOutput, viewModel: viewModel)
                                .frame(maxHeight: 200)
                        }
                    }
                    .padding()
                }
            }
            
            Spacer()
        }
    }
}
