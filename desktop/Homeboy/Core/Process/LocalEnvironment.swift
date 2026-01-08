import Foundation

/// Utility for detecting Local by Flywheel's PHP and MySQL paths
/// Auto-detects the latest versions and builds environment for WP-CLI execution
struct LocalEnvironment {
    
    static let servicesPath = NSHomeDirectory() + "/Library/Application Support/Local/lightning-services"
    
    /// Detects the latest PHP bin directory from Local by Flywheel
    static func detectPHPBinDirectory() -> String? {
        detectBinDirectory(prefix: "php-")
    }
    
    /// Detects the latest MySQL bin directory from Local by Flywheel
    static func detectMySQLBinDirectory() -> String? {
        detectBinDirectory(prefix: "mysql-")
    }
    
    /// Detects bin directory for a given service prefix (php-, mysql-)
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
    
    /// Builds a complete environment dictionary with PHP and MySQL in PATH
    /// Returns nil if PHP cannot be detected (MySQL is optional)
    static func buildEnvironment() -> [String: String]? {
        guard let phpBin = detectPHPBinDirectory() else {
            return nil
        }
        
        let mysqlBin = detectMySQLBinDirectory()
        
        var env = ProcessInfo.processInfo.environment
        
        // Build PATH with PHP (required), MySQL (optional), and system paths
        var pathComponents = [phpBin]
        if let mysqlBin = mysqlBin {
            pathComponents.append(mysqlBin)
        }
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
    
    /// Returns detected MySQL version string (e.g., "8.0.35") or nil
    static func detectedMySQLVersion() -> String? {
        guard let mysqlBin = detectMySQLBinDirectory() else { return nil }
        
        // Extract version from path like ".../mysql-8.0.35+4/bin/darwin-arm64/bin"
        let components = mysqlBin.components(separatedBy: "/")
        for component in components {
            if component.hasPrefix("mysql-") {
                // mysql-8.0.35+4 -> 8.0.35
                let version = component
                    .replacingOccurrences(of: "mysql-", with: "")
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
