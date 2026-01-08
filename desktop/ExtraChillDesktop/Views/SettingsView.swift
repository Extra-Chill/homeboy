import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var authManager: AuthManager
    @AppStorage("sendyListId") private var sendyListId = ""
    @AppStorage("localWPPath") private var localWPPath = "/Users/chubes/Developer/LocalWP/testing-grounds/app/public"
    @AppStorage("lastUsedTag") private var lastUsedTag = ""
    
    // Deployment paths
    @AppStorage("extraChillBasePath") private var extraChillBasePath = "/Users/chubes/Developer/Extra Chill Platform"
    @AppStorage("dataMachineBasePath") private var dataMachineBasePath = "/Users/chubes/Developer/Data Machine Ecosystem"
    
    @State private var phpVersion: String?
    @State private var mysqlVersion: String?
    @State private var testResult: (success: Bool, message: String)?
    @State private var isTesting = false
    
    // Cloudways settings
    @State private var cloudwaysHost = ""
    @State private var cloudwaysUsername = ""
    @State private var cloudwaysAppPath = ""
    @State private var hasSSHKey = false
    @State private var publicKey: String?
    @State private var isGeneratingKey = false
    @State private var isTestingCloudways = false
    @State private var cloudwaysTestResult: (success: Bool, message: String)?
    
    // Live MySQL settings
    @State private var liveMySQLUsername = ""
    @State private var liveMySQLPassword = ""
    @State private var liveMySQLDatabase = ""
    @State private var liveMySQLSaveResult: (success: Bool, message: String)?
    
    var body: some View {
        Form {
            Section("Account") {
                if let user = authManager.user {
                    LabeledContent("Logged in as", value: user.displayName)
                    LabeledContent("Username", value: user.username)
                    
                    Button("Logout", role: .destructive) {
                        authManager.logout()
                    }
                } else {
                    Text("Not logged in")
                        .foregroundColor(.secondary)
                }
            }
            
            Section("Newsletter") {
                TextField("Sendy List ID", text: $sendyListId)
                    .textFieldStyle(.roundedBorder)
                Text("Enter the Sendy list ID for bulk email subscriptions")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("WP-CLI") {
                TextField("Local WP Path", text: $localWPPath)
                    .textFieldStyle(.roundedBorder)
                Text("Path to your Local WP site's public directory")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                // Detected Local by Flywheel paths
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
                
                // Test result display
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
            
            Section("Bandcamp Scraper") {
                TextField("Default Tag", text: $lastUsedTag)
                    .textFieldStyle(.roundedBorder)
                Text("Default Bandcamp tag to use (e.g., 'south-carolina', 'lo-fi')")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("Deployment Paths") {
                TextField("Extra Chill Platform", text: $extraChillBasePath)
                    .textFieldStyle(.roundedBorder)
                Text("Local path to the Extra Chill Platform repository")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                TextField("Data Machine Ecosystem", text: $dataMachineBasePath)
                    .textFieldStyle(.roundedBorder)
                Text("Local path to the Data Machine Ecosystem repository")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("Database (Cloudways)") {
                TextField("MySQL Username", text: $liveMySQLUsername)
                    .textFieldStyle(.roundedBorder)
                SecureField("MySQL Password", text: $liveMySQLPassword)
                    .textFieldStyle(.roundedBorder)
                TextField("Database Name", text: $liveMySQLDatabase)
                    .textFieldStyle(.roundedBorder)
                Text("Credentials for your Cloudways MySQL database. Connection uses SSH tunnel with credentials above.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                HStack {
                    Button("Save Credentials") {
                        saveLiveMySQLCredentials()
                    }
                    .disabled(liveMySQLUsername.isEmpty || liveMySQLPassword.isEmpty || liveMySQLDatabase.isEmpty)
                }
                
                if let result = liveMySQLSaveResult {
                    HStack {
                        Image(systemName: result.success ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundColor(result.success ? .green : .red)
                        Text(result.message)
                            .font(.caption)
                            .foregroundColor(result.success ? .green : .red)
                    }
                }
            }
            
            Section("Cloudways Deployment") {
                TextField("Server Host", text: $cloudwaysHost)
                    .textFieldStyle(.roundedBorder)
                TextField("SSH Username", text: $cloudwaysUsername)
                    .textFieldStyle(.roundedBorder)
                TextField("Application Path", text: $cloudwaysAppPath)
                    .textFieldStyle(.roundedBorder)
                Text("e.g., /applications/extrachill_main/public_html")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                HStack {
                    Button("Save Credentials") {
                        saveCloudwaysCredentials()
                    }
                    .disabled(cloudwaysHost.isEmpty || cloudwaysUsername.isEmpty || cloudwaysAppPath.isEmpty)
                }
                
                Divider()
                
                // SSH Key Section
                Text("SSH Key Authentication")
                    .font(.headline)
                
                if hasSSHKey {
                    HStack {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundColor(.green)
                        Text("SSH key configured")
                        Spacer()
                        Button("Show Public Key") {
                            showPublicKey()
                        }
                        .buttonStyle(.borderless)
                        Button("Remove Key", role: .destructive) {
                            removeSSHKey()
                        }
                        .buttonStyle(.borderless)
                    }
                } else {
                    Text("Generate an SSH key pair to enable passwordless deployment.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    Button("Generate SSH Key") {
                        generateSSHKey()
                    }
                    .disabled(isGeneratingKey)
                    
                    if isGeneratingKey {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                
                if let key = publicKey {
                    GroupBox("Public Key (add to ~/.ssh/authorized_keys on server)") {
                        ScrollView {
                            Text(key)
                                .font(.system(.caption, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .frame(height: 60)
                        
                        Button("Copy to Clipboard") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(key, forType: .string)
                        }
                        .buttonStyle(.borderless)
                    }
                }
                
                Divider()
                
                // Test connection
                HStack {
                    Button("Test Connection") {
                        testCloudwaysConnection()
                    }
                    .disabled(!hasSSHKey || isTestingCloudways || cloudwaysHost.isEmpty)
                    
                    if isTestingCloudways {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                
                if let result = cloudwaysTestResult {
                    HStack {
                        Image(systemName: result.success ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundColor(result.success ? .green : .red)
                        Text(result.message)
                            .font(.caption)
                            .foregroundColor(result.success ? .green : .red)
                    }
                }
            }
            
            Section("About") {
                LabeledContent("Version", value: "0.2.0")
                LabeledContent("Build", value: "5")
            }
        }
        .formStyle(.grouped)
        .padding()
        .frame(minWidth: 500)
        .onAppear {
            detectLocalPaths()
            loadCloudwaysSettings()
            loadMySQLSettings()
        }
    }
    
    private func detectLocalPaths() {
        phpVersion = LocalEnvironment.detectedPHPVersion()
        mysqlVersion = LocalEnvironment.detectedMySQLVersion()
    }
    
    // MARK: - Cloudways Settings
    
    private func loadCloudwaysSettings() {
        let creds = KeychainService.getCloudwaysCredentials()
        cloudwaysHost = creds.host ?? ""
        cloudwaysUsername = creds.username ?? ""
        cloudwaysAppPath = creds.appPath ?? ""
        hasSSHKey = KeychainService.hasSSHKey()
    }
    
    private func saveCloudwaysCredentials() {
        KeychainService.storeCloudwaysCredentials(
            host: cloudwaysHost,
            username: cloudwaysUsername,
            appPath: cloudwaysAppPath
        )
        cloudwaysTestResult = (true, "Credentials saved")
    }
    
    private func generateSSHKey() {
        isGeneratingKey = true
        publicKey = nil
        
        SSHService.generateSSHKeyPair { result in
            isGeneratingKey = false
            switch result {
            case .success(let keys):
                hasSSHKey = true
                publicKey = keys.publicKey
            case .failure(let error):
                cloudwaysTestResult = (false, "Key generation failed: \(error.localizedDescription)")
            }
        }
    }
    
    private func showPublicKey() {
        publicKey = KeychainService.getSSHKeyPair().publicKey
    }
    
    private func removeSSHKey() {
        KeychainService.clearSSHKeys()
        
        // Also remove from disk
        try? FileManager.default.removeItem(atPath: SSHService.defaultKeyPath)
        try? FileManager.default.removeItem(atPath: SSHService.defaultPublicKeyPath)
        
        hasSSHKey = false
        publicKey = nil
    }
    
    private func testCloudwaysConnection() {
        guard let sshService = SSHService() else {
            cloudwaysTestResult = (false, "SSH not configured")
            return
        }
        
        isTestingCloudways = true
        cloudwaysTestResult = nil
        
        sshService.testConnection { result in
            isTestingCloudways = false
            switch result {
            case .success(let output):
                cloudwaysTestResult = (true, output)
            case .failure(let error):
                cloudwaysTestResult = (false, error.localizedDescription)
            }
        }
    }
    
    private func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Select your Local WP site's public directory"
        
        if panel.runModal() == .OK, let url = panel.url {
            localWPPath = url.path
        }
    }
    
    private func testWPCLI() {
        guard let environment = LocalEnvironment.buildEnvironment() else {
            testResult = (false, "PHP not detected")
            return
        }
        
        isTesting = true
        testResult = nil
        
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            process.currentDirectoryURL = URL(fileURLWithPath: localWPPath)
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
    
    // MARK: - MySQL Settings
    
    private func loadMySQLSettings() {
        let liveCreds = KeychainService.getLiveMySQLCredentials()
        liveMySQLUsername = liveCreds.username ?? ""
        liveMySQLPassword = liveCreds.password ?? ""
        liveMySQLDatabase = liveCreds.database ?? ""
    }
    
    private func saveLiveMySQLCredentials() {
        do {
            try KeychainService.storeLiveMySQLCredentials(
                username: liveMySQLUsername,
                password: liveMySQLPassword,
                database: liveMySQLDatabase
            )
            liveMySQLSaveResult = (true, "Credentials saved")
        } catch {
            liveMySQLSaveResult = (false, "Failed to save: \(error.localizedDescription)")
        }
    }
}

#Preview {
    SettingsView()
        .environmentObject(AuthManager())
}
