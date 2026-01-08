import SwiftUI

struct WordPressSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    @EnvironmentObject var authManager: AuthManager
    
    @State private var phpVersion: String?
    @State private var mysqlVersion: String?
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
            Section("WP-CLI (Local Development)") {
                TextField("Local WP Path", text: Binding(
                    get: { config.activeProject.localDev.wpCliPath },
                    set: { newValue in
                        config.activeProject.localDev.wpCliPath = newValue
                        config.saveActiveProject()
                    }
                ))
                .textFieldStyle(.roundedBorder)
                Text("Path to your Local WP site's public directory")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                LabeledContent("PHP Version") {
                    if let version = phpVersion {
                        Text(version)
                            .foregroundColor(.secondary)
                    } else {
                        Text("Not detected")
                            .foregroundColor(.red)
                    }
                }
                
                LabeledContent("MySQL Version") {
                    if let version = mysqlVersion {
                        Text(version)
                            .foregroundColor(.secondary)
                    } else {
                        Text("Not detected")
                            .foregroundColor(.orange)
                    }
                }
                
                HStack {
                    Button("Browse...") {
                        selectFolder()
                    }
                    
                    Button("Test Connection") {
                        testWPCLI()
                    }
                    .disabled(isTesting || phpVersion == nil)
                }
                
                if let result = testResult {
                    HStack {
                        Image(systemName: result.success ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundColor(result.success ? .green : .red)
                        Text(result.message)
                            .font(.caption)
                            .foregroundColor(result.success ? .green : .red)
                    }
                }
            }
            
            Section("Multisite Configuration") {
                Toggle("Multisite Network", isOn: Binding(
                    get: { config.activeProject.multisite?.enabled ?? false },
                    set: { newValue in
                        if newValue {
                            if config.activeProject.multisite == nil {
                                config.activeProject.multisite = MultisiteConfig(enabled: true)
                            } else {
                                config.activeProject.multisite?.enabled = true
                            }
                        } else {
                            config.activeProject.multisite?.enabled = false
                        }
                        config.saveActiveProject()
                    }
                ))
                
                if config.activeProject.multisite?.enabled == true {
                    TextField("Table Prefix", text: Binding(
                        get: { config.activeProject.multisite?.tablePrefix ?? "wp_" },
                        set: { newValue in
                            config.activeProject.multisite?.tablePrefix = newValue
                            config.saveActiveProject()
                        }
                    ))
                    .textFieldStyle(.roundedBorder)
                    Text("Database table prefix (e.g., wp_, c8c_)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    if let blogs = config.activeProject.multisite?.blogs, !blogs.isEmpty {
                        Text("\(blogs.count) site(s) configured")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    
                    Text("Edit multisite blogs in the JSON config file for now.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            
            Section("REST API Authentication") {
                Toggle("Enable API Authentication", isOn: $apiEnabled)
                    .onChange(of: apiEnabled) { _, newValue in
                        config.activeProject.api.enabled = newValue
                        config.saveActiveProject()
                    }
                
                if apiEnabled {
                    TextField("API Base URL", text: $apiBaseURL)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: apiBaseURL) { _, newValue in
                            config.activeProject.api.baseURL = newValue
                            config.saveActiveProject()
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
                            Text(error)
                                .font(.caption)
                                .foregroundColor(.red)
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
            detectLocalPaths()
            loadAPISettings()
        }
    }
    
    private func detectLocalPaths() {
        phpVersion = LocalEnvironment.detectedPHPVersion()
        mysqlVersion = LocalEnvironment.detectedMySQLVersion()
    }
    
    private func loadAPISettings() {
        apiEnabled = config.activeProject.api.enabled
        apiBaseURL = config.activeProject.api.baseURL
    }
    
    private func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Select your Local WP site's public directory"
        
        if panel.runModal() == .OK, let url = panel.url {
            config.activeProject.localDev.wpCliPath = url.path
            config.saveActiveProject()
        }
    }
    
    private func testWPCLI() {
        guard let environment = LocalEnvironment.buildEnvironment() else {
            testResult = (false, "PHP not detected")
            return
        }
        
        isTesting = true
        testResult = nil
        
        let wpCliPath = config.activeProject.localDev.wpCliPath
        
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            process.currentDirectoryURL = URL(fileURLWithPath: wpCliPath)
            process.executableURL = URL(fileURLWithPath: "/opt/homebrew/bin/wp")
            process.arguments = ["core", "version"]
            process.environment = environment
            
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
                loginError = authManager.error ?? "Login failed"
            }
        }
    }
}
