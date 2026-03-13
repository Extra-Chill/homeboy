import Foundation

/// Version target matching CLI's VersionTarget struct
struct VersionTarget: Codable {
    var file: String
    var pattern: String?
}

/// Extension configuration scoped to a component
/// Mirrors CLI's ScopedExtensionConfig
/// Settings use [String: Any] for flexibility - extension defines its own schema
struct ScopedExtensionConfig: Codable {
    var version: String?
    var settings: [String: String]?  // Simplified: key-value string pairs
}

/// Standalone component configuration stored in ~/.config/homeboy/components/{id}.json
/// Components are reusable across projects - projects reference components by ID.
/// Aligned with CLI's Component struct (CLI is authoritative).
struct ComponentConfiguration: Codable, Identifiable {
    var id: String
    var aliases: [String]              // NEW: Multiple aliases for component
    var localPath: String
    var remotePath: String
    var buildArtifact: String?
    var extensions: [String: ScopedExtensionConfig]?  // NEW: Extension configs by ID
    var versionTargets: [VersionTarget]?
    var changelogTarget: String?       // NEW: Dedicated changelog file path
    var hooks: [String: [String]]?     // NEW: Lifecycle hooks

    /// Display name computed from id (e.g., "chubes-theme" -> "Chubes Theme")
    var displayName: String {
        id.split(separator: "-")
            .map { $0.capitalized }
            .joined(separator: " ")
    }

    /// First version file from version_targets (for backward compat with UI)
    var versionFile: String? {
        versionTargets?.first?.file
    }

    /// First version pattern from version_targets (for backward compat with UI)
    var versionPattern: String? {
        versionTargets?.first?.pattern
    }

    /// Get configured extension IDs
    var extensionIds: [String] {
        extensions.map { Array($0.keys) } ?? []
    }

    /// Check if component has a specific extension configured
    func hasExtension(_ id: String) -> Bool {
        extensions?.keys.contains(id) ?? false
    }
}
