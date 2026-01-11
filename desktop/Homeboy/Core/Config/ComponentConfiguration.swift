import Foundation

/// Standalone component configuration stored in ~/Library/Application Support/Homeboy/components/{id}.json
/// Components are reusable across projects - projects reference components by ID.
struct ComponentConfiguration: Codable, Identifiable {
    var id: String
    var name: String
    var localPath: String
    var remotePath: String
    var buildArtifact: String
    var versionFile: String?
    var versionPattern: String?
    var buildCommand: String?
    var isNetwork: Bool?

    init(
        id: String,
        name: String,
        localPath: String,
        remotePath: String,
        buildArtifact: String,
        versionFile: String? = nil,
        versionPattern: String? = nil,
        buildCommand: String? = nil,
        isNetwork: Bool? = nil
    ) {
        self.id = id
        self.name = name
        self.localPath = localPath
        self.remotePath = remotePath
        self.buildArtifact = buildArtifact
        self.versionFile = versionFile
        self.versionPattern = versionPattern
        self.buildCommand = buildCommand
        self.isNetwork = isNetwork
    }
}
