import SwiftUI

struct DatabaseSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    
    @State private var liveMySQLPassword = ""
    @FocusState private var passwordFieldFocused: Bool
    
    var body: some View {
        Form {
            Section("Remote Database") {
                TextField("MySQL Username", text: Binding(
                    get: { config.safeActiveProject.database.user },
                    set: { newValue in
                        Task {
                            try? await config.updateActiveProject { $0.database.user = newValue }
                        }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                
                SecureField("MySQL Password", text: $liveMySQLPassword)
                    .textFieldStyle(.roundedBorder)
                    .focused($passwordFieldFocused)
                    .onChange(of: passwordFieldFocused) { _, isFocused in
                        if !isFocused && !liveMySQLPassword.isEmpty {
                            savePasswordToKeychain()
                        }
                    }
                
                TextField("Database Name", text: Binding(
                    get: { config.safeActiveProject.database.name },
                    set: { newValue in
                        Task {
                            try? await config.updateActiveProject { $0.database.name = newValue }
                        }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                
                Text("Credentials for your remote MySQL database. Connection uses SSH tunnel.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            loadPassword()
        }
    }
    
    private func loadPassword() {
        let liveCreds = KeychainService.getLiveMySQLCredentials()
        liveMySQLPassword = liveCreds.password ?? ""
    }
    
    private func savePasswordToKeychain() {
        try? KeychainService.storeLiveMySQLCredentials(
            username: config.safeActiveProject.database.user,
            password: liveMySQLPassword,
            database: config.safeActiveProject.database.name
        )
    }
}
