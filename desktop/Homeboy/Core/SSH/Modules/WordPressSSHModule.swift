import Foundation

/// WordPress-specific SSH operations for WP-CLI execution.
/// Self-contained module used by the WP-CLI Terminal.
class WordPressSSHModule {
    private let ssh: SSHService
    private let pathResolver: RemotePathResolver

    /// Path to wp-content directory
    var wpContentPath: String { pathResolver.wpContentPath }

    /// Path to themes directory
    var themesPath: String { pathResolver.themesPath }

    /// Path to plugins directory
    var pluginsPath: String { pathResolver.pluginsPath }

    /// WordPress root directory
    var wpRootPath: String { pathResolver.wpRootPath }

    // MARK: - Initialization

    /// Initialize with an SSHService and base path
    init?(ssh: SSHService, basePath: String) {
        guard !basePath.isEmpty else { return nil }
        self.ssh = ssh
        self.pathResolver = RemotePathResolver(basePath: basePath)
    }

    /// Initialize from active project configuration
    init?() {
        let project = ConfigurationManager.readCurrentProject()

        guard project.isWordPress,
              let resolver = RemotePathResolver(project: project),
              let sshService = SSHService() else {
            return nil
        }

        self.ssh = sshService
        self.pathResolver = resolver
    }

    // MARK: - Validation

    /// Validate that the wp-content path is a valid WordPress wp-content directory
    func validateWPContentPath() async throws -> Bool {
        let themesExists = try await ssh.isDirectory(themesPath)
        let pluginsExists = try await ssh.isDirectory(pluginsPath)
        return themesExists && pluginsExists
    }

    /// Get validation status with details
    func getValidationStatus() async -> WPContentValidationStatus {
        do {
            let themesExists = try await ssh.isDirectory(themesPath)
            let pluginsExists = try await ssh.isDirectory(pluginsPath)

            if themesExists && pluginsExists {
                return .valid
            } else if !themesExists && !pluginsExists {
                return .invalid("Missing themes/ and plugins/ directories")
            } else if !themesExists {
                return .invalid("Missing themes/ directory")
            } else {
                return .invalid("Missing plugins/ directory")
            }
        } catch {
            return .error(error.localizedDescription)
        }
    }

    // MARK: - WP-CLI

    /// Execute a WP-CLI command on the remote server
    func executeWPCLI(
        _ command: String,
        onOutput: ((String) -> Void)? = nil,
        onComplete: @escaping (Result<String, Error>) -> Void
    ) {
        let fullCommand = "cd '\(wpRootPath)' && wp \(command)"
        ssh.executeCommand(fullCommand, onOutput: onOutput, onComplete: onComplete)
    }

    /// Get WordPress version via WP-CLI
    func getWordPressVersion(onComplete: @escaping (Result<String, Error>) -> Void) {
        executeWPCLI("core version") { result in
            switch result {
            case .success(let output):
                onComplete(.success(output.trimmingCharacters(in: .whitespacesAndNewlines)))
            case .failure(let error):
                onComplete(.failure(error))
            }
        }
    }
}

// MARK: - Validation Status

enum WPContentValidationStatus {
    case valid
    case invalid(String)
    case error(String)

    var isValid: Bool {
        if case .valid = self { return true }
        return false
    }

    var message: String {
        switch self {
        case .valid: return "Valid wp-content directory"
        case .invalid(let msg): return msg
        case .error(let msg): return "Error: \(msg)"
        }
    }
}
