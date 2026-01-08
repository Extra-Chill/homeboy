import SwiftUI

/// Sheet for creating or editing a grouping
struct GroupingEditorSheet: View {
    @Environment(\.dismiss) private var dismiss
    
    let mode: Mode
    let onSave: (String) -> Void
    
    @State private var name: String = ""
    @FocusState private var isNameFocused: Bool
    
    enum Mode {
        case create
        case rename(ItemGrouping)
        
        var title: String {
            switch self {
            case .create: return "New Group"
            case .rename: return "Rename Group"
            }
        }
        
        var buttonTitle: String {
            switch self {
            case .create: return "Create"
            case .rename: return "Save"
            }
        }
    }
    
    var body: some View {
        VStack(spacing: 16) {
            Text(mode.title)
                .font(.headline)
            
            TextField("Group name", text: $name)
                .textFieldStyle(.roundedBorder)
                .focused($isNameFocused)
                .onSubmit {
                    if isValid {
                        save()
                    }
                }
            
            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.escape)
                
                Spacer()
                
                Button(mode.buttonTitle) {
                    save()
                }
                .keyboardShortcut(.return)
                .disabled(!isValid)
                .buttonStyle(.borderedProminent)
            }
        }
        .padding()
        .frame(width: 300)
        .onAppear {
            if case .rename(let grouping) = mode {
                name = grouping.name
            }
            isNameFocused = true
        }
    }
    
    private var isValid: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    
    private func save() {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else { return }
        onSave(trimmedName)
        dismiss()
    }
}

#Preview("Create") {
    GroupingEditorSheet(mode: .create) { name in
        print("Created group: \(name)")
    }
}

#Preview("Rename") {
    GroupingEditorSheet(
        mode: .rename(ItemGrouping(name: "Network Tables"))
    ) { name in
        print("Renamed to: \(name)")
    }
}
