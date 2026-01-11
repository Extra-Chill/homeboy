import Foundation

/// Checks CLI installation status and available updates via GitHub releases
actor CLIVersionChecker {
    static let shared = CLIVersionChecker()

    private let githubRepo = "Extra-Chill/homeboy-cli"

    // Cache the latest version for 1 hour to avoid excessive API calls
    private var cachedLatestVersion: String?
    private var cacheTimestamp: Date?
    private let cacheDuration: TimeInterval = 3600

    // Cache the CLI path to avoid repeated filesystem checks
    private var cachedCLIPath: String?

    /// Known installation paths in priority order
    private static let knownPaths: [String] = [
        "/opt/homebrew/bin/homeboy",      // Apple Silicon Homebrew
        "/usr/local/bin/homeboy",         // Intel Homebrew / manual
        NSString(string: "~/.cargo/bin/homeboy").expandingTildeInPath  // Cargo install
    ]

    struct VersionInfo: Sendable {
        let installed: String?
        let latest: String?
        let path: String?
        let isInstalled: Bool
        let updateAvailable: Bool

        var statusText: String {
            if !isInstalled {
                return "Not installed"
            }
            if updateAvailable, let installed = installed, let latest = latest {
                return "Update available: \(installed) → \(latest)"
            }
            if let installed = installed {
                return "v\(installed) (up to date)"
            }
            return "Installed"
        }
    }

    /// Find CLI path by checking known installation locations
    nonisolated private static func findCLIPath() -> String? {
        for path in knownPaths {
            if FileManager.default.isExecutableFile(atPath: path) {
                return path
            }
        }
        return nil
    }

    /// Find CLI path with caching for repeated calls
    func cliPath() -> String? {
        if let cached = cachedCLIPath {
            return cached
        }

        if let path = Self.findCLIPath() {
            cachedCLIPath = path
            return path
        }

        return nil
    }

    /// Clear cached CLI path (call after user claims to have installed)
    func clearCache() {
        cachedCLIPath = nil
    }

    /// Check if CLI binary is installed (nonisolated for synchronous access)
    nonisolated var isInstalled: Bool {
        Self.findCLIPath() != nil
    }

    /// Get installed version by running `homeboy --version`
    func installedVersion() async -> String? {
        guard let path = cliPath() else { return nil }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = ["--version"]

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        do {
            try process.run()
            process.waitUntilExit()

            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            guard let output = String(data: data, encoding: .utf8) else { return nil }

            // Parse "homeboy 0.1.0" → "0.1.0"
            let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.lowercased().hasPrefix("homeboy ") {
                return String(trimmed.dropFirst(8))
            }
            return trimmed
        } catch {
            return nil
        }
    }

    /// Get latest version from GitHub releases API
    func latestVersion(forceRefresh: Bool = false) async -> String? {
        // Return cached version if still valid
        if !forceRefresh,
           let cached = cachedLatestVersion,
           let timestamp = cacheTimestamp,
           Date().timeIntervalSince(timestamp) < cacheDuration {
            return cached
        }

        let urlString = "https://api.github.com/repos/\(githubRepo)/releases/latest"
        guard let url = URL(string: urlString) else { return nil }

        var request = URLRequest(url: url)
        request.setValue("application/vnd.github.v3+json", forHTTPHeaderField: "Accept")
        request.timeoutInterval = 10

        do {
            let (data, response) = try await URLSession.shared.data(for: request)

            guard let httpResponse = response as? HTTPURLResponse,
                  httpResponse.statusCode == 200 else {
                return cachedLatestVersion
            }

            struct GitHubRelease: Decodable {
                let tag_name: String
            }

            let release = try JSONDecoder().decode(GitHubRelease.self, from: data)
            // Parse "v0.1.0" → "0.1.0"
            var version = release.tag_name
            if version.hasPrefix("v") {
                version = String(version.dropFirst())
            }

            // Update cache
            cachedLatestVersion = version
            cacheTimestamp = Date()

            return version
        } catch {
            return cachedLatestVersion
        }
    }

    /// Check full version info including update availability
    func checkForUpdate() async -> VersionInfo {
        async let installedTask = installedVersion()
        async let latestTask = latestVersion()

        let installed = await installedTask
        let latest = await latestTask
        let path = cliPath()
        let isInstalled = path != nil

        let updateAvailable: Bool
        if let installed = installed, let latest = latest {
            updateAvailable = compareVersions(installed, latest) < 0
        } else {
            updateAvailable = false
        }

        return VersionInfo(
            installed: installed,
            latest: latest,
            path: path,
            isInstalled: isInstalled,
            updateAvailable: updateAvailable
        )
    }

    /// Compare semantic versions: returns -1 if v1 < v2, 0 if equal, 1 if v1 > v2
    private func compareVersions(_ v1: String, _ v2: String) -> Int {
        let parts1 = v1.split(separator: ".").compactMap { Int($0) }
        let parts2 = v2.split(separator: ".").compactMap { Int($0) }

        let maxLength = max(parts1.count, parts2.count)

        for i in 0..<maxLength {
            let p1 = i < parts1.count ? parts1[i] : 0
            let p2 = i < parts2.count ? parts2[i] : 0

            if p1 < p2 { return -1 }
            if p1 > p2 { return 1 }
        }

        return 0
    }
}
