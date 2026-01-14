import AppKit
import SwiftUI

@main
struct HomeboyApp: App {
    @StateObject private var authManager = AuthManager()
    @State private var showCLISetup = false
    @State private var showCLIUpdate = false
    @State private var cliVersionInfo: CLIVersionChecker.VersionInfo?

    init() {
        ConfigurationManager.shared.syncBundledProjectTypes()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(authManager)
                .task {
                    if !CLIVersionChecker.shared.isInstalled {
                        showCLISetup = true
                    } else {
                        // CLI installed - load projects and check for updates
                        await ConfigurationManager.shared.loadProjectsFromCLI()

                        let info = await CLIVersionChecker.shared.checkForUpdate()
                        cliVersionInfo = info
                        if info.updateAvailable {
                            showCLIUpdate = true
                        }
                    }
                }
                .sheet(isPresented: $showCLISetup) {
                    CLISetupSheet(isPresented: $showCLISetup)
                }
                .sheet(isPresented: $showCLIUpdate) {
                    CLIUpdateSheet(isPresented: $showCLIUpdate, versionInfo: cliVersionInfo)
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1000, height: 700)

        Settings {
            SettingsView()
                .environmentObject(authManager)
        }
    }
}

// MARK: - CLI Setup Sheet

struct CLISetupSheet: View {
    @Binding var isPresented: Bool
    @State private var isChecking = false

    private let installCommands = "brew tap extra-chill/tap\nbrew install homeboy"

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "terminal")
                .font(.system(size: 48))
                .foregroundColor(.accentColor)

            Text("CLI Setup Required")
                .font(.title2)
                .fontWeight(.semibold)

            Text("Homeboy requires the CLI tool for deployment, database, and remote operations.")
                .multilineTextAlignment(.center)
                .foregroundColor(.secondary)
                .frame(maxWidth: 400)

            VStack(alignment: .leading, spacing: 8) {
                Text("Install via Homebrew:")
                    .font(.subheadline)
                    .foregroundColor(.secondary)

                HStack {
                    Text(installCommands)
                        .font(.system(.caption, design: .monospaced))
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(nsColor: .textBackgroundColor))
                        .cornerRadius(8)

                    Button {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(installCommands, forType: .string)
                    } label: {
                        Image(systemName: "doc.on.doc")
                    }
                    .buttonStyle(.borderless)
                    .help("Copy commands")
                }
            }
            .frame(maxWidth: 400)

            HStack(spacing: 16) {
                Button("Skip for Now") {
                    isPresented = false
                }
                .buttonStyle(.bordered)

                Button {
                    checkAndDismiss()
                } label: {
                    HStack {
                        if isChecking {
                            ProgressView()
                                .scaleEffect(0.7)
                                .frame(width: 14, height: 14)
                        }
                        Text("I've Installed It")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isChecking)
            }

            Text("You can also install later via Settings → General")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding(32)
        .frame(width: 500)
    }

    private func checkAndDismiss() {
        isChecking = true

        Task {
            // Clear cached path so we re-scan for newly installed CLI
            await CLIVersionChecker.shared.clearCache()

            // Give a moment for filesystem to update
            try? await Task.sleep(nanoseconds: 500_000_000)

            await MainActor.run {
                isChecking = false
                if CLIVersionChecker.shared.isInstalled {
                    isPresented = false
                }
            }
        }
    }
}

// MARK: - CLI Update Sheet

struct CLIUpdateSheet: View {
    @Binding var isPresented: Bool
    let versionInfo: CLIVersionChecker.VersionInfo?

    @State private var isUpgrading = false
    @State private var upgradeOutput = ""
    @State private var upgradeError: (any DisplayableError)?

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "arrow.up.circle.fill")
                .font(.system(size: 48))
                .foregroundColor(.blue)

            Text("CLI Update Available")
                .font(.title2)
                .fontWeight(.semibold)

            if let info = versionInfo,
               let installed = info.installed,
               let latest = info.latest {
                Text("Version \(installed) → \(latest)")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }

            if let error = upgradeError {
                InlineErrorView(error)
            } else if isUpgrading {
                VStack(spacing: 12) {
                    ProgressView()
                    Text("Upgrading...")
                        .foregroundColor(.secondary)
                    if !upgradeOutput.isEmpty {
                        Text(upgradeOutput)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundColor(.secondary)
                            .lineLimit(2)
                    }
                }
            } else {
                HStack(spacing: 16) {
                    Button("Skip") {
                        isPresented = false
                    }
                    .buttonStyle(.bordered)

                    Button("Upgrade Now") {
                        upgradeCLI()
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
        }
        .padding(32)
        .frame(width: 400)
    }

    private func upgradeCLI() {
        isUpgrading = true
        upgradeOutput = ""
        upgradeError = nil

        Task {
            let success = await runBrewUpgrade()
            await MainActor.run {
                isUpgrading = false
                if success {
                    isPresented = false
                }
            }
        }
    }

    private func runBrewUpgrade() async -> Bool {
        let brewPaths = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        guard let brewPath = brewPaths.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else {
            await MainActor.run { upgradeError = AppError("Homebrew not found", source: "CLI Upgrade") }
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

            let handle = pipe.fileHandleForReading
            for try await line in handle.bytes.lines {
                await MainActor.run { upgradeOutput = line }
            }

            process.waitUntilExit()

            if process.terminationStatus == 0 {
                await CLIVersionChecker.shared.clearCache()
            }

            return process.terminationStatus == 0
        } catch {
            await MainActor.run { upgradeError = error.toDisplayableError(source: "CLI Upgrade") }
            return false
        }
    }
}
