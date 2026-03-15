import SwiftUI

/// Reusable console output view with copy/clear controls
struct ExtensionConsoleView: View {
    @Binding var output: String
    @ObservedObject var viewModel: ExtensionViewModel
    
    private var extensionName: String {
        ExtensionManager.shared.loadedExtension(withId: viewModel.extensionId)?.name ?? viewModel.extensionId
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Toolbar
            HStack {
                Spacer()
                
                CopyButton.console(output, source: "Extension: \(extensionName)")
                    .disabled(output.isEmpty)
                
                Button {
                    viewModel.clearConsole()
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.plain)
                .help("Clear console")
            }
            .padding(.horizontal)
            .padding(.vertical, 4)
            
            // Console output
            ScrollViewReader { proxy in
                ScrollView {
                    Text(output.isEmpty ? "Ready to run..." : output)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(output.isEmpty ? .secondary : .primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .id("console-bottom")
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: output) { _, _ in
                    withAnimation {
                        proxy.scrollTo("console-bottom", anchor: .bottom)
                    }
                }
            }
        }
        .frame(minHeight: 150)
    }
}
