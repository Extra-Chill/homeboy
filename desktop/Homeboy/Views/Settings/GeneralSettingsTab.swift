import SwiftUI

struct GeneralSettingsTab: View {
    @ObservedObject var config: ConfigurationManager
    @ObservedObject private var cliInstaller = CLIInstaller.shared
    
    var body: some View {
        Form {
            Section("Project Information") {
                LabeledContent("Project ID", value: config.activeProject.id)
                
                TextField("Project Name", text: Binding(
                    get: { config.activeProject.name },
                    set: { newValue in
                        config.activeProject.name = newValue
                        config.saveActiveProject()
                    }
                ))
                .textFieldStyle(.roundedBorder)
                
                LabeledContent("Domain", value: config.activeProject.domain)
                
                Text("Project ID and Domain cannot be changed after creation.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section("Command Line Tool") {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Terminal Command")
                            .font(.headline)
                        Text(cliInstaller.isInstalled ? "Installed at /usr/local/bin/homeboy" : "Not installed")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    Spacer()
                    Button(cliInstaller.isInstalled ? "Uninstall" : "Install") {
                        if cliInstaller.isInstalled {
                            cliInstaller.uninstall()
                        } else {
                            cliInstaller.install()
                        }
                    }
                }
            }
            
            Section("About") {
                LabeledContent("Version", value: "0.3.0")
                LabeledContent("Build", value: "6")
            }
        }
        .formStyle(.grouped)
    }
}
