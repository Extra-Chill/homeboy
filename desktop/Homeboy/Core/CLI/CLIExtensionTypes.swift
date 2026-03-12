import Foundation

/// Extension entry from `homeboy extension list --json`
/// Matches the CLI's ExtensionEntry struct output
struct CLIExtensionEntry: Decodable, Identifiable {
    let id: String
    let name: String
    let version: String
    let description: String
    let runtime: String  // "executable" or "platform"
    let compatible: Bool
    let ready: Bool
    let configured: Bool
    let linked: Bool
    let path: String  // Extension directory path for manifest reading
    let actions: [CLIExtensionAction]?  // Present when extension defines actions
}

struct CLIExtensionAction: Decodable {
    let id: String
    let label: String
    let type: String
}

/// Response wrapper for `homeboy extension list --json`
struct CLIExtensionListResponse: Decodable {
    let success: Bool
    let data: CLIExtensionListData?
    let error: CLIErrorDetail?
}

struct CLIExtensionListData: Decodable {
    let projectId: String?
    let extensions: [CLIExtensionEntry]
}

/// Standard CLI error detail
struct CLIErrorDetail: Decodable {
    let code: String
    let message: String
}

/// Response wrapper for extension install/uninstall operations
struct CLIExtensionOperationResponse: Decodable {
    let success: Bool
    let data: CLIExtensionOperationData?
    let error: CLIErrorDetail?
}

struct CLIExtensionOperationData: Decodable {
    let extensionId: String
    let path: String?
    let url: String?
}
