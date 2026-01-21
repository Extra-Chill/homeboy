import Foundation

/// Version target matching CLI's VersionTarget struct
struct VersionTarget: Codable {
    var file: String
    var pattern: String?
}

/// Standalone component configuration stored in ~/.config/homeboy/components/{id}.json
/// Components are reusable across projects - projects reference components by ID.
/// Aligned with CLI's Component struct (CLI is authoritative).
struct ComponentConfiguration: Codable, Identifiable {
    var id: String
    var localPath: String
    var remotePath: String
    var buildArtifact: String?
    var versionTargets: [VersionTarget]?
    var buildCommand: String?

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
}
