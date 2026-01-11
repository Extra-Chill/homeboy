import AppKit
import SwiftUI

struct GeneralSettingsTab: View {
    @ObservedObject var config: ConfigurationManager

    @State private var editedName: String = ""
    @State private var renameError: String?
    @State private var isEditing: Bool = false

    @State private var cliVersionInfo: CLIVersionChecker.VersionInfo?
    @State private var isCheckingVersion: Bool = false
    @State private var versionCheckError: AppError?
    @State private var isUpgrading: Bool = false
    @State private var upgradeOutput: String = ""

    private let installCommands = "brew tap extra-chill/tap\nbrew install homeboy"
    private let upgradeCommand = "brew upgrade homeboy"

    var body: some View {
        Form {
            Section("Project Information") {
                LabeledContent("Project ID", value: config.safeActiveProject.id)

                HStack {
                    TextField("Project Name", text: $editedName, onCommit: commitRename)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: editedName) { _, newValue in
                            isEditing = newValue != config.safeActiveProject.name
                            if renameError != nil { renameError = nil }
                        }

                    if isEditing {
                        Button("Save") {
                            commitRename()
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(editedName.isEmpty)
                    }
                }

                if let error = renameError {
                    Text(error)
                        .font(.caption)
                        .foregroundColor(.red)
                }

                TextField("Domain", text: Binding(
                    get: { config.safeActiveProject.domain },
                    set: { newValue in
                        config.updateActiveProject { $0.domain = newValue }
                    }
                ))
                .textFieldStyle(.roundedBorder)

                Text("Used for display and identification (e.g., example.com)")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("Command Line Tool") {
                cliStatusView
            }

            Section("About") {
                LabeledContent("Version", value: ContentContext.appVersion)
                LabeledContent("Build", value: ContentContext.appBuild)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            editedName = config.safeActiveProject.name
            checkCLIVersion()
        }
        .onChange(of: config.activeProject?.id) { _, _ in
            editedName = config.safeActiveProject.name
            isEditing = false
            renameError = nil
        }
    }

    @ViewBuilder
    private var cliStatusView: some View {
        if isCheckingVersion {
            HStack {
                ProgressView()
                    .scaleEffect(0.7)
                Text("Checking CLI status...")
                    .foregroundColor(.secondary)
            }
        } else if let error = versionCheckError {
            InlineErrorView(error)
        } else if let info = cliVersionInfo {
            if !info.isInstalled {
                cliNotInstalledView
            } else if info.updateAvailable {
                cliUpdateAvailableView(info: info)
            } else {
                cliUpToDateView(info: info)
            }
        } else {
            cliNotInstalledView
        }
    }

    private var cliNotInstalledView: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.orange)
                Text("CLI Not Installed")
                    .font(.headline)
            }

            Text("Install via Homebrew:")
                .font(.subheadline)
                .foregroundColor(.secondary)

            HStack {
                Text(installCommands)
                    .font(.system(.caption, design: .monospaced))
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color(nsColor: .textBackgroundColor))
                    .cornerRadius(6)

                Button {
                    copyToClipboard(installCommands)
                } label: {
                    Image(systemName: "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .help("Copy commands")
            }

            Button("Check Again") {
                checkCLIVersion()
            }
            .buttonStyle(.bordered)
        }
    }

    private func cliUpdateAvailableView(info: CLIVersionChecker.VersionInfo) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "arrow.up.circle.fill")
                    .foregroundColor(.blue)
                Text("Update Available")
                    .font(.headline)
            }

            if let installed = info.installed, let latest = info.latest {
                Text("Installed: \(installed) → Latest: \(latest)")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }

            if isUpgrading {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        ProgressView()
                            .scaleEffect(0.7)
                        Text("Upgrading...")
                            .foregroundColor(.secondary)
                    }
                    if !upgradeOutput.isEmpty {
                        Text(upgradeOutput)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundColor(.secondary)
                            .lineLimit(3)
                    }
                }
            } else {
                HStack(spacing: 12) {
                    Button("Upgrade Now") {
                        upgradeCLI()
                    }
                    .buttonStyle(.borderedProminent)

                    Button {
                        copyToClipboard(upgradeCommand)
                    } label: {
                        Image(systemName: "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                    .help("Copy command: \(upgradeCommand)")

                    Button {
                        checkCLIVersion()
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.bordered)
                    .help("Check again")
                }
            }
        }
    }

    private func cliUpToDateView(info: CLIVersionChecker.VersionInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("CLI Installed")
                    .font(.headline)
                Spacer()
                Button {
                    checkCLIVersion()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Check for updates")
            }

            if let version = info.installed {
                Text("Version \(version)")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }

            if let path = info.path {
                Text(path)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(.secondary)
            }
        }
    }

    private func checkCLIVersion() {
        isCheckingVersion = true
        versionCheckError = nil

        Task {
            let info = await CLIVersionChecker.shared.checkForUpdate()
            await MainActor.run {
                cliVersionInfo = info
                isCheckingVersion = false
            }
        }
    }

    private func upgradeCLI() {
        isUpgrading = true
        upgradeOutput = ""

        Task {
            let result = await runBrewUpgrade()
            await MainActor.run {
                isUpgrading = false
                upgradeOutput = ""
                if result {
                    // Clear cache and recheck version
                    Task {
                        await CLIVersionChecker.shared.clearCache()
                        checkCLIVersion()
                    }
                }
            }
        }
    }

    private func runBrewUpgrade() async -> Bool {
        // Find brew executable
        let brewPaths = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        guard let brewPath = brewPaths.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else {
            await MainActor.run {
                upgradeOutput = "Homebrew not found"
            }
            return false
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: brewPath)
        process.arguments = ["upgrade", "homeboy"]

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        do {
            try process.run()

            // Read output asynchronously
            let handle = pipe.fileHandleForReading
            for try await line in handle.bytes.lines {
                await MainActor.run {
                    upgradeOutput = line
                }
            }

            process.waitUntilExit()
            return process.terminationStatus == 0
        } catch {
            await MainActor.run {
                upgradeOutput = "Failed: \(error.localizedDescription)"
            }
            return false
        }
    }

    private func copyToClipboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func commitRename() {
        guard let project = config.activeProject else { return }
        guard !editedName.isEmpty else { return }

        let result = config.renameProject(project, to: editedName)
        switch result {
        case .success:
            isEditing = false
            renameError = nil
        case .failure(let error):
            renameError = error.localizedDescription
        }
    }
}
