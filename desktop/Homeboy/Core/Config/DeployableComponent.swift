import Foundation

enum DeployStatus: Equatable {
    case current
    case needsUpdate
    case notDeployed
    case buildRequired
    case unknown
    case deploying
    case failed(String)

    var icon: String {
        switch self {
        case .current: return "checkmark.circle.fill"
        case .needsUpdate: return "arrow.up.circle.fill"
        case .notDeployed: return "xmark.circle.fill"
        case .buildRequired: return "hammer.fill"
        case .unknown: return "questionmark.circle"
        case .deploying: return "arrow.triangle.2.circlepath"
        case .failed: return "exclamationmark.triangle.fill"
        }
    }

    var color: String {
        switch self {
        case .current: return "green"
        case .needsUpdate: return "orange"
        case .notDeployed: return "red"
        case .buildRequired: return "yellow"
        case .unknown: return "gray"
        case .deploying: return "blue"
        case .failed: return "red"
        }
    }
}

struct DeployableComponent: Identifiable, Hashable {
    let id: String
    let name: String
    let localPath: String
    let remotePath: String
    let buildArtifact: String?
    let versionFile: String?
    let versionPattern: String?
    let buildCommand: String?

    var buildArtifactPath: String? {
        guard let artifact = buildArtifact else { return nil }
        return "\(localPath)/\(artifact)"
    }

    var versionFilePath: String? {
        guard let vf = versionFile else { return nil }
        return "\(localPath)/\(vf)"
    }

    var artifactExtension: String {
        guard let artifact = buildArtifact else { return "" }
        return (artifact as NSString).pathExtension.lowercased()
    }

    var hasBuildArtifact: Bool {
        guard let path = buildArtifactPath else { return false }
        return FileManager.default.fileExists(atPath: path)
    }

    var hasBuildCommand: Bool {
        buildCommand != nil && !buildCommand!.isEmpty
    }

    init(from config: ComponentConfiguration) {
        self.id = config.id
        self.name = config.displayName
        self.localPath = config.localPath
        self.remotePath = config.remotePath
        self.buildArtifact = config.buildArtifact
        self.versionFile = config.versionFile
        self.versionPattern = config.versionPattern
        self.buildCommand = config.buildCommand
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }

    static func == (lhs: DeployableComponent, rhs: DeployableComponent) -> Bool {
        lhs.id == rhs.id
    }
}

// MARK: - Version Info

enum VersionInfo: Equatable {
    case version(String)
    case timestamp(Date)
    case notDeployed
    case parseError(String)

    var displayString: String {
        switch self {
        case .version(let v):
            return v
        case .timestamp(let date):
            let formatter = DateFormatter()
            formatter.dateStyle = .medium
            formatter.timeStyle = .none
            return formatter.string(from: date)
        case .notDeployed:
            return "—"
        case .parseError:
            return "Error"
        }
    }

    var errorMessage: String? {
        if case .parseError(let message) = self { return message }
        return nil
    }
}
