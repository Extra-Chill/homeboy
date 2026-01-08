import SwiftUI

/// Renders action buttons from module manifest
struct ModuleActionsBar: View {
    let module: LoadedModule
    @ObservedObject var viewModel: ModuleViewModel
    @EnvironmentObject var authManager: AuthManager
    
    var body: some View {
        HStack {
            ForEach(module.manifest.actions) { action in
                actionButton(for: action)
            }
            
            Spacer()
            
            if let result = viewModel.actionResult {
                Text(result)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            if let error = viewModel.error {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
                    .lineLimit(2)
            }
        }
        .padding()
    }
    
    @ViewBuilder
    private func actionButton(for action: ActionConfig) -> some View {
        let state = buttonState(for: action)
        
        Button {
            Task {
                await viewModel.performAction(action, module: module)
            }
        } label: {
            HStack(spacing: 4) {
                Image(systemName: iconForAction(action))
                Text(action.label)
            }
        }
        .buttonStyle(.bordered)
        .disabled(!state.isEnabled)
        .help(state.tooltip)
    }
    
    /// Determines button state based on auth, settings, and selection requirements
    private func buttonState(for action: ActionConfig) -> (isEnabled: Bool, tooltip: String) {
        // Always disabled while performing action
        if viewModel.isPerformingAction {
            return (false, "Action in progress...")
        }
        
        // Check row selection for selectable outputs
        if module.manifest.output.selectable && viewModel.selectedRows.isEmpty {
            return (false, "Select rows first")
        }
        
        // For API actions, check additional requirements
        if action.type == .api {
            // Check auth requirement
            if action.requiresAuth == true && !authManager.isAuthenticated {
                return (false, "Login required - configure in Settings > API")
            }
            
            // Check if required settings are configured
            if let missingSettings = missingRequiredSettings(for: action) {
                return (false, "Configure \(missingSettings) in Settings > Modules")
            }
        }
        
        return (true, "")
    }
    
    /// Returns comma-separated list of missing settings used in action payload, or nil if all present
    private func missingRequiredSettings(for action: ActionConfig) -> String? {
        guard let payload = action.payload else { return nil }
        
        var missing: [String] = []
        
        for (_, value) in payload {
            if case .string(let template) = value,
               template.hasPrefix("{{settings.") && template.hasSuffix("}}") {
                // Extract setting key
                let key = String(template.dropFirst(11).dropLast(2))
                
                // Check if setting has a value
                let value = module.settings.string(for: key)
                if value == nil || value!.isEmpty {
                    // Find the label from manifest
                    if let setting = module.manifest.settings.first(where: { $0.id == key }) {
                        missing.append(setting.label)
                    } else {
                        missing.append(key)
                    }
                }
            }
        }
        
        return missing.isEmpty ? nil : missing.joined(separator: ", ")
    }
    
    private func iconForAction(_ action: ActionConfig) -> String {
        switch action.builtin {
        case .copyColumn:
            return "doc.on.doc"
        case .exportCsv:
            return "square.and.arrow.down"
        case .copyJson:
            return "doc.on.doc"
        case .none:
            // API actions
            if action.id.contains("newsletter") || action.id.contains("subscribe") {
                return "envelope"
            }
            return "arrow.right.circle"
        }
    }
}
