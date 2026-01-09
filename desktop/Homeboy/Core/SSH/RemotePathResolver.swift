import Foundation

/// Single source of truth for all remote path construction.
/// Ensures consistent path handling across GUI and CLI.
struct RemotePathResolver {

    // MARK: - Core Path Joining

    /// Join two path segments, normalizing slashes
    static func join(_ base: String, _ relative: String) -> String {
        let trimmedBase = base.hasSuffix("/") ? String(base.dropLast()) : base
        let trimmedRelative = relative.hasPrefix("/") ? String(relative.dropFirst()) : relative
        return "\(trimmedBase)/\(trimmedRelative)"
    }

    /// Normalize a path (remove double slashes, ensure no trailing slash)
    static func normalize(_ path: String) -> String {
        var result = path.replacingOccurrences(of: "//", with: "/")
        while result.hasSuffix("/") && result.count > 1 {
            result = String(result.dropLast())
        }
        return result
    }

    /// Get parent directory of a path
    static func parent(of path: String) -> String {
        (path as NSString).deletingLastPathComponent
    }

    /// Get filename from a path
    static func filename(of path: String) -> String {
        (path as NSString).lastPathComponent
    }

    /// Ensure path has trailing slash (for directory listings)
    static func withTrailingSlash(_ path: String) -> String {
        path.hasSuffix("/") ? path : "\(path)/"
    }

    // MARK: - Project-Based Resolution

    let basePath: String

    init(basePath: String) {
        self.basePath = Self.normalize(basePath)
    }

    init?(project: ProjectConfiguration) {
        guard let basePath = project.basePath, !basePath.isEmpty else {
            return nil
        }
        self.basePath = Self.normalize(basePath)
    }

    /// Resolve a relative path against the base path
    func resolve(_ relativePath: String) -> String {
        Self.join(basePath, relativePath)
    }

    // MARK: - Component Deployment Paths

    /// Full remote directory for a component
    func componentDirectory(for component: DeployableComponent) -> String {
        resolve(component.remotePath)
    }

    /// Parent directory of component (where zips are uploaded)
    func componentParent(for component: DeployableComponent) -> String {
        Self.parent(of: componentDirectory(for: component))
    }

    /// Artifact upload destination
    func artifactUploadPath(for component: DeployableComponent) -> String {
        let parent = componentParent(for: component)
        let artifactName = Self.filename(of: component.buildArtifact)
        return Self.join(parent, artifactName)
    }

    /// Temporary extraction directory
    func tempDeployPath(for component: DeployableComponent) -> String {
        Self.join(componentParent(for: component), "__temp_deploy__")
    }

    /// Version file path
    func versionFilePath(for component: DeployableComponent) -> String? {
        guard let versionFile = component.versionFile else { return nil }
        return Self.join(componentDirectory(for: component), versionFile)
    }

    /// Staging upload path (in user's home directory for reliable SCP).
    /// SCP with SFTP protocol has issues with deep nested paths on some hosts.
    func stagingUploadPath(for component: DeployableComponent) -> String {
        let artifactName = "\(component.id).\(component.artifactExtension)"
        return "tmp/\(artifactName)"
    }

    // MARK: - File/Log Paths

    /// Resolve a relative file path (for remote file editor)
    func filePath(_ relativePath: String) -> String {
        resolve(relativePath)
    }

    /// Resolve a relative log path (for remote log viewer)
    func logPath(_ relativePath: String) -> String {
        resolve(relativePath)
    }
}
