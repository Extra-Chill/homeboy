import Foundation

/// Single source of truth for SSH key file operations.
/// Keychain/UserDefaults storage remains in KeychainService.
struct SSHKeyManager {

    // MARK: - Paths

    static func privateKeyPath(forServer serverId: String) -> String {
        AppPaths.key(forServer: serverId).path
    }

    static func publicKeyPath(forServer serverId: String) -> String {
        privateKeyPath(forServer: serverId) + ".pub"
    }

    static var keysDirectory: URL {
        AppPaths.keys
    }

    // MARK: - File Operations

    static func hasKeyFile(forServer serverId: String) -> Bool {
        FileManager.default.fileExists(atPath: privateKeyPath(forServer: serverId))
    }

    static func ensureKeysDirectoryExists() throws {
        try FileManager.default.createDirectory(at: keysDirectory, withIntermediateDirectories: true)
    }

    static func readPublicKey(forServer serverId: String) throws -> String {
        try String(contentsOfFile: publicKeyPath(forServer: serverId), encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func readPrivateKey(forServer serverId: String) throws -> String {
        try String(contentsOfFile: privateKeyPath(forServer: serverId), encoding: .utf8)
    }

    static func writeKeyPair(forServer serverId: String, privateKey: String, publicKey: String) throws {
        try ensureKeysDirectoryExists()

        let privPath = privateKeyPath(forServer: serverId)
        let pubPath = publicKeyPath(forServer: serverId)

        try privateKey.write(toFile: privPath, atomically: true, encoding: .utf8)
        try publicKey.write(toFile: pubPath, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: privPath)
    }

    static func deleteKeyFiles(forServer serverId: String) {
        try? FileManager.default.removeItem(atPath: privateKeyPath(forServer: serverId))
        try? FileManager.default.removeItem(atPath: publicKeyPath(forServer: serverId))
    }

    /// Restore key file from Keychain if missing from disk
    static func restoreFromKeychainIfNeeded(forServer serverId: String) -> Bool {
        if hasKeyFile(forServer: serverId) {
            return true
        }

        guard let privateKey = KeychainService.getSSHKeyPair(forServer: serverId).privateKey else {
            return false
        }

        do {
            try ensureKeysDirectoryExists()
            let privPath = privateKeyPath(forServer: serverId)
            try privateKey.write(toFile: privPath, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: privPath)
            return true
        } catch {
            return false
        }
    }
}
