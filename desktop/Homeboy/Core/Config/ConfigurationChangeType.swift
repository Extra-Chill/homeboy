import Foundation

/// Fields trackable for projectModified granularity.
/// Allows ViewModels to react only to relevant configuration changes.
enum ProjectField: String, CaseIterable {
    case server
    case basePath
    case database
    case components
    case subTargets
    case remoteFiles
    case remoteLogs
    case api
    case tools
}

/// Typed configuration changes with associated context.
/// Single source of truth for all configuration change events in the app.
enum ConfigurationChangeType {
    // Project lifecycle
    case projectWillSwitch(from: String?, to: String)
    case projectDidSwitch(projectId: String)
    case projectModified(projectId: String, fields: Set<ProjectField>)

    // Server changes
    case serverAdded(serverId: String)
    case serverModified(serverId: String)
    case serverRemoved(serverId: String)

    // Extension changes
    case extensionAdded(extensionId: String)
    case extensionModified(extensionId: String)
    case extensionRemoved(extensionId: String)

    // Project type changes
    case projectTypeModified(typeId: String)
}
