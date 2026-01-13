import SwiftUI

struct WordPressSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    @EnvironmentObject var authManager: AuthManager

    // API Auth state
    @State private var apiEnabled = false
    @State private var apiBaseURL = ""
    @State private var loginUsername = ""
    @State private var loginPassword = ""
    @State private var isLoggingIn = false
    @State private var loginError: String?

    var body: some View {
        Form {
            Section("Database") {
                TextField("Table Prefix", text: Binding(
                    get: { config.safeActiveProject.tablePrefix ?? "wp_" },
                    set: { newValue in
                        config.updateActiveProject { $0.tablePrefix = newValue }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                Text("Database table prefix (e.g., wp_, c8c_)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("Multisite Configuration") {
                if config.safeActiveProject.hasSubTargets {
                    Text("\(config.safeActiveProject.subTargets.count) site(s) configured")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    ForEach(config.safeActiveProject.subTargets) { subTarget in
                        HStack {
                            if subTarget.isDefault {
                                Image(systemName: "star.fill")
                                    .foregroundColor(.yellow)
                                    .font(.caption)
                            }
                            Text(subTarget.name)
                            Spacer()
                            Text(subTarget.domain)
                                .foregroundColor(.secondary)
                                .font(.caption)
                        }
                    }
                } else {
                    Text("No multisite configured")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                Text("Use the CLI to manage subtargets: homeboy project subtarget add")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("REST API Authentication") {
                Toggle("Enable API Authentication", isOn: $apiEnabled)
                    .onChange(of: apiEnabled) { _, newValue in
                        config.updateActiveProject { $0.api.enabled = newValue }
                    }

                if apiEnabled {
                    TextField("API Base URL", text: $apiBaseURL)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: apiBaseURL) { _, newValue in
                            config.updateActiveProject { $0.api.baseURL = newValue }
                        }
                    Text("e.g., https://yoursite.com/wp-json/extrachill/v1")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    Divider()

                    if authManager.isAuthenticated, let user = authManager.user {
                        HStack {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundColor(.green)
                            Text("Logged in as \(user.displayName)")
                            Spacer()
                            Button("Logout", role: .destructive) {
                                authManager.logout()
                            }
                        }
                    } else {
                        TextField("Username", text: $loginUsername)
                            .textFieldStyle(.roundedBorder)
                        SecureField("Password", text: $loginPassword)
                            .textFieldStyle(.roundedBorder)

                        HStack {
                            Button("Login") {
                                performLogin()
                            }
                            .disabled(loginUsername.isEmpty || loginPassword.isEmpty || apiBaseURL.isEmpty || isLoggingIn)

                            if isLoggingIn {
                                ProgressView()
                                    .controlSize(.small)
                            }
                        }

                        if let error = loginError {
                            InlineErrorView(error, source: "WordPress Settings")
                        }
                    }
                }

                Text("Requires JWT token authentication. Standard WordPress login is not supported.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            loadAPISettings()
        }
    }

    private func loadAPISettings() {
        apiEnabled = config.safeActiveProject.api.enabled
        apiBaseURL = config.safeActiveProject.api.baseURL
    }

    private func performLogin() {
        isLoggingIn = true
        loginError = nil

        Task {
            await authManager.login(identifier: loginUsername, password: loginPassword)
            isLoggingIn = false

            if authManager.isAuthenticated {
                loginUsername = ""
                loginPassword = ""
            } else {
                loginError = authManager.error?.body ?? "Login failed"
            }
        }
    }
}
