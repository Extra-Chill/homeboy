import SwiftUI

/// Dynamically generates input form from module manifest
struct ModuleInputsView: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            // Network site selector for WP-CLI modules with multisite
            if viewModel.isWPCLIModule && viewModel.isMultisite {
                LabeledContent("Network Site") {
                    Picker("", selection: networkSiteBinding) {
                        ForEach(viewModel.networkSites, id: \.blogId) { site in
                            Text("\(site.name) (\(site.blogId))").tag(site.name.lowercased())
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(maxWidth: 200)
                }
            }
            
            ForEach(module.manifest.inputs) { input in
                inputField(for: input)
            }
            
            HStack {
                Button {
                    viewModel.run(module: module)
                } label: {
                    HStack {
                        if viewModel.isRunning {
                            ProgressView()
                                .scaleEffect(0.7)
                                .frame(width: 16, height: 16)
                        } else {
                            Image(systemName: "play.fill")
                        }
                        Text(viewModel.isRunning ? "Running..." : "Run")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.isRunning || viewModel.isSettingUp)
                
                if let error = viewModel.error {
                    Text(error)
                        .font(.caption)
                        .foregroundColor(.red)
                        .lineLimit(2)
                }
            }
        }
        .padding()
    }
    
    private var networkSiteBinding: Binding<String> {
        Binding(
            get: { viewModel.selectedNetworkSite ?? "main" },
            set: { viewModel.selectedNetworkSite = $0 }
        )
    }
    
    @ViewBuilder
    private func inputField(for input: InputConfig) -> some View {
        switch input.type {
        case .text:
            LabeledContent(input.label) {
                TextField(input.placeholder ?? "", text: binding(for: input.id))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 300)
            }
            
        case .stepper:
            LabeledContent(input.label) {
                HStack {
                    TextField("", text: binding(for: input.id))
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 60)
                    
                    Stepper("", value: intBinding(for: input.id, min: input.min ?? 0, max: input.max ?? 100))
                        .labelsHidden()
                }
            }
            
        case .toggle:
            Toggle(input.label, isOn: boolBinding(for: input.id))
            
        case .select:
            if let options = input.options {
                LabeledContent(input.label) {
                    Picker("", selection: binding(for: input.id)) {
                        ForEach(options) { option in
                            Text(option.label).tag(option.value)
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(maxWidth: 200)
                }
            }
        }
    }
    
    // MARK: - Bindings
    
    private func binding(for id: String) -> Binding<String> {
        Binding(
            get: { viewModel.inputValues[id] ?? "" },
            set: { viewModel.inputValues[id] = $0 }
        )
    }
    
    private func intBinding(for id: String, min: Int, max: Int) -> Binding<Int> {
        Binding(
            get: {
                Int(viewModel.inputValues[id] ?? "0") ?? 0
            },
            set: {
                let clamped = Swift.min(Swift.max($0, min), max)
                viewModel.inputValues[id] = String(clamped)
            }
        )
    }
    
    private func boolBinding(for id: String) -> Binding<Bool> {
        Binding(
            get: {
                let value = viewModel.inputValues[id] ?? "false"
                return value.lowercased() == "true" || value == "1"
            },
            set: {
                viewModel.inputValues[id] = $0 ? "true" : "false"
            }
        )
    }
}
