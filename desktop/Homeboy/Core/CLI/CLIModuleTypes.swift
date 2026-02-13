import Foundation

/// Module entry from `homeboy module list --json`
/// Matches the CLI's ModuleEntry struct output
struct CLIModuleEntry: Decodable, Identifiable {
    let id: String
    let name: String
    let version: String
    let description: String
    let runtime: String  // "executable" or "platform"
    let compatible: Bool
    let ready: Bool
    let configured: Bool
    let linked: Bool
    let path: String  // Module directory path for manifest reading
    let actions: [CLIModuleAction]?  // Present when module defines actions
}

struct CLIModuleAction: Decodable {
    let id: String
    let label: String
    let type: String
}

/// Response wrapper for `homeboy module list --json`
struct CLIModuleListResponse: Decodable {
    let success: Bool
    let data: CLIModuleListData?
    let error: CLIErrorDetail?
}

struct CLIModuleListData: Decodable {
    let projectId: String?
    let modules: [CLIModuleEntry]
}

/// Standard CLI error detail
struct CLIErrorDetail: Decodable {
    let code: String
    let message: String
}

/// Response wrapper for module install/uninstall operations
struct CLIModuleOperationResponse: Decodable {
    let success: Bool
    let data: CLIModuleOperationData?
    let error: CLIErrorDetail?
}

struct CLIModuleOperationData: Decodable {
    let moduleId: String
    let path: String?
    let url: String?
}
