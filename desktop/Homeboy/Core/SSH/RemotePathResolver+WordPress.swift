import Foundation

/// WordPress-specific path resolution extensions.
/// Use only when `project.isWordPress == true`.
extension RemotePathResolver {

    /// wp-content directory
    var wpContentPath: String {
        resolve("wp-content")
    }

    /// WordPress root (same as basePath for WordPress projects)
    var wpRootPath: String {
        basePath
    }

    /// Plugins directory
    var pluginsPath: String {
        Self.join(wpContentPath, "plugins")
    }

    /// Themes directory
    var themesPath: String {
        Self.join(wpContentPath, "themes")
    }
}
