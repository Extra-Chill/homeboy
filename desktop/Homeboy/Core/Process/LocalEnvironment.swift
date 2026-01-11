import Foundation

/// Utility for detecting Local by Flywheel's PHP and MySQL paths
/// Auto-detects the latest versions and builds environment for WP-CLI execution
struct LocalEnvironment {
    
    static let servicesPath = NSHomeDirectory() + "/Library/Application Support/Local/lightning-services"
    
    /// Detects the latest PHP bin directory from Local by Flywheel
    static func detectPHPBinDirectory() -> String? {
        detectBinDirectory(prefix: "php-")
    }
    
    /// Detects bin directory for a given service prefix (php-)
    private static func detectBinDirectory(prefix: String) -> String? {
        let fileManager = FileManager.default
        
        guard let contents = try? fileManager.contentsOfDirectory(atPath: servicesPath) else {
            return nil
        }
        
        // Filter for matching directories, sort descending to get latest version
        // localizedStandardCompare handles version strings correctly (8.4.10 > 8.2.27)
        let dirs = contents
            .filter { $0.hasPrefix(prefix) }
            .sorted { $0.localizedStandardCompare($1) == .orderedDescending }
        
        // Check for the binary in each directory, trying arm64 first then x86_64
        for dir in dirs {
            for arch in ["darwin-arm64", "darwin-x86_64"] {
                let binPath = "\(servicesPath)/\(dir)/bin/\(arch)/bin"
                if fileManager.fileExists(atPath: binPath) {
                    return binPath
                }
            }
        }
        
        return nil
    }
    
    /// Builds a complete environment dictionary with PHP in PATH.
    /// Returns nil if PHP cannot be detected.
    static func buildEnvironment() -> [String: String]? {
        guard let phpBin = detectPHPBinDirectory() else {
            return nil
        }

        var env = ProcessInfo.processInfo.environment

        var pathComponents = [phpBin]
        pathComponents.append(contentsOf: ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])

        env["PATH"] = pathComponents.joined(separator: ":")

        return env
    }
    
    /// Returns detected PHP version string (e.g., "8.4.10") or nil
    static func detectedPHPVersion() -> String? {
        guard let phpBin = detectPHPBinDirectory() else { return nil }
        
        // Extract version from path like ".../php-8.4.10+0/bin/darwin-arm64/bin"
        let components = phpBin.components(separatedBy: "/")
        for component in components {
            if component.hasPrefix("php-") {
                // php-8.4.10+0 -> 8.4.10
                let version = component
                    .replacingOccurrences(of: "php-", with: "")
                    .components(separatedBy: "+")
                    .first
                return version
            }
        }
        return nil
    }
    
    /// Checks if Local by Flywheel services are available
    static var isAvailable: Bool {
        detectPHPBinDirectory() != nil
    }
}
