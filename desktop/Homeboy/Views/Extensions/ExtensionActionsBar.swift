import SwiftUI

/// Renders action buttons from extension manifest
struct ExtensionActionsBar: View {
    let currentExtension: LoadedExtension
    @ObservedObject var viewModel: ExtensionViewModel
    
    var body: some View {
        HStack {
            ForEach(currentExtension.manifest.actions ?? []) { action in
                actionButton(for: action)
            }
            
            Spacer()
            
            if let result = viewModel.actionResult {
                Text(result)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            if let error = viewModel.error {
                InlineErrorView(error)
            }
        }
        .padding()
    }
    
    @ViewBuilder
    private func actionButton(for action: ActionConfig) -> some View {
        let state = buttonState(for: action)
        
        Button {
            Task {
                await viewModel.performAction(action, extension: currentExtension)
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
    
    /// Determines button state based on selection requirements
    private func buttonState(for action: ActionConfig) -> (isEnabled: Bool, tooltip: String) {
        // Always disabled while performing action
        if viewModel.isPerformingAction {
            return (false, "Action in progress...")
        }
        
        // Check row selection for selectable outputs
        if currentExtension.manifest.output?.selectable == true && viewModel.selectedRows.isEmpty {
            return (false, "Select rows first")
        }
        
        return (true, "")
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
            return "arrow.right.circle"
        }
    }
}
