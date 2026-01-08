import Foundation
import AppKit

/// Manages installation of the `homeboy` CLI tool via symlink to /usr/local/bin
@MainActor
class CLIInstaller: ObservableObject {
    static let shared = CLIInstaller()
    
    static let symlinkPath = "/usr/local/bin/homeboy"
    private static let hasPromptedKey = "CLIInstallPrompted"
    
    @Published private(set) var isInstalled: Bool = false
    
    /// Path to the CLI binary inside the app bundle
    var cliPath: String {
        Bundle.main.bundlePath + "/Contents/MacOS/homeboy-cli"
    }
    
    private init() {
        refreshInstallStatus()
    }
    
    /// Check if symlink exists and points to our CLI binary
    func refreshInstallStatus() {
        let fm = FileManager.default
        guard let target = try? fm.destinationOfSymbolicLink(atPath: Self.symlinkPath) else {
            isInstalled = false
            return
        }
        isInstalled = target == cliPath
    }
    
    /// Whether user has been prompted to install (stored permanently)
    var hasPrompted: Bool {
        get { UserDefaults.standard.bool(forKey: Self.hasPromptedKey) }
        set { UserDefaults.standard.set(newValue, forKey: Self.hasPromptedKey) }
    }
    
    /// Show install dialog if CLI is not installed and user hasn't been prompted
    func promptInstallIfNeeded() {
        refreshInstallStatus()
        guard !isInstalled && !hasPrompted else { return }
        
        let alert = NSAlert()
        alert.messageText = "Install Command Line Tool?"
        alert.informativeText = "Install the 'homeboy' command for terminal access.\n\nThis allows you to run commands like:\nhomeboy wp extrachill plugin list"
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Install")
        alert.addButton(withTitle: "Not Now")
        
        let response = alert.runModal()
        hasPrompted = true
        
        if response == .alertFirstButtonReturn {
            install()
        }
    }
    
    /// Create symlink with admin privileges via osascript
    @discardableResult
    func install() -> Bool {
        let script = "do shell script \"ln -sf '\(cliPath)' '\(Self.symlinkPath)'\" with administrator privileges"
        
        guard let appleScript = NSAppleScript(source: script) else {
            refreshInstallStatus()
            return false
        }
        
        var error: NSDictionary?
        appleScript.executeAndReturnError(&error)
        
        refreshInstallStatus()
        return error == nil && isInstalled
    }
    
    /// Remove symlink with admin privileges via osascript
    @discardableResult
    func uninstall() -> Bool {
        let script = "do shell script \"rm -f '\(Self.symlinkPath)'\" with administrator privileges"
        
        guard let appleScript = NSAppleScript(source: script) else {
            refreshInstallStatus()
            return false
        }
        
        var error: NSDictionary?
        appleScript.executeAndReturnError(&error)
        
        refreshInstallStatus()
        return error == nil && !isInstalled
    }
}
