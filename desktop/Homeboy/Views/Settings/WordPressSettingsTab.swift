import SwiftUI

struct WordPressSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    @EnvironmentObject var authManager: AuthManager

    @State private var testResult: (success: Bool, message: String)?
    @State private var isTesting = false
    
    // API Auth state
    @State private var apiEnabled = false
    @State private var apiBaseURL = ""
    @State private var loginUsername = ""
    @State private var loginPassword = ""
    @State private var isLoggingIn = false
    @State private var loginError: String?
    
    var body: some View {
        Form {
            Section("Local CLI") {
                TextField("Local Site Path", text: Binding(
                    get: { config.safeActiveProject.localEnvironment.sitePath },
                    set: { newValue in
                        config.updateActiveProject { $0.localEnvironment.sitePath = newValue }
                    }
                ))
                .textFieldStyle(.roundedBorder)
                Text("Path to your local site's root directory")
                    .font(.caption)
                    .foregroundColor(.secondary)

                HStack {
                    Button("Browse...") {
                        selectFolder()
                    }

                    Button("Test WP-CLI") {
                        testWPCLI()
                    }
                    .disabled(isTesting || config.safeActiveProject.localEnvironment.sitePath.isEmpty)
                }
                
                if let result = testResult {
                    if result.success {
                        HStack {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundColor(.green)
                            Text(result.message)
                                .font(.caption)
                                .foregroundColor(.green)
                        }
                    } else {
                        InlineErrorView(result.message, source: "WP-CLI Test")
                    }
                }
            }
            
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
    
    private func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Select your local site's root directory"
        
        if panel.runModal() == .OK, let url = panel.url {
            config.updateActiveProject { $0.localEnvironment.sitePath = url.path }
        }
    }
    
    private func testWPCLI() {
        isTesting = true
        testResult = nil

        let sitePath = config.safeActiveProject.localEnvironment.sitePath

        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            process.currentDirectoryURL = URL(fileURLWithPath: sitePath)
            process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
            process.arguments = ["core", "version"]

            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe

            do {
                try process.run()
                process.waitUntilExit()

                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

                DispatchQueue.main.async {
                    isTesting = false
                    if process.terminationStatus == 0 {
                        testResult = (true, "WordPress \(output)")
                    } else {
                        testResult = (false, output.isEmpty ? "Command failed" : output)
                    }
                }
            } catch {
                DispatchQueue.main.async {
                    isTesting = false
                    testResult = (false, error.localizedDescription)
                }
            }
        }
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
