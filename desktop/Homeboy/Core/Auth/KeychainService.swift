import Foundation
import Security

enum KeychainError: Error {
    case duplicateItem
    case itemNotFound
    case unexpectedStatus(OSStatus)
    case invalidData
}

struct KeychainService {
    private static let service = "com.extrachill.homeboy"
    
    private enum Keys {
        static let accessToken = "accessToken"
        static let refreshToken = "refreshToken"
        static let sshPrivateKey = "sshPrivateKey"
        static let liveMySQLPassword = "liveMySQLPassword"
    }
    
    // Non-sensitive data stored in UserDefaults to avoid Keychain prompts
    private enum UserDefaultsKeys {
        static let accessExpiresAt = "accessExpiresAt"
        static let deviceId = "deviceId"
        static let sshPublicKey = "sshPublicKey"
        // MySQL credentials (non-sensitive parts)
        static let liveMySQLUsername = "liveMySQLUsername"
        static let liveMySQLDatabase = "liveMySQLDatabase"
    }
    
    // MARK: - Token Storage (Site-Namespaced)
    
    static func storeTokens(for siteId: String, accessToken: String, refreshToken: String, expiresAt: Date) throws {
        try store(key: "\(Keys.accessToken)_\(siteId)", value: accessToken)
        try store(key: "\(Keys.refreshToken)_\(siteId)", value: refreshToken)
        UserDefaults.standard.set(expiresAt.timeIntervalSince1970, forKey: "\(UserDefaultsKeys.accessExpiresAt)_\(siteId)")
    }
    
    static func getTokens(for siteId: String) -> (accessToken: String?, refreshToken: String?, expiresAt: Date?) {
        let accessToken = try? retrieve(key: "\(Keys.accessToken)_\(siteId)")
        let refreshToken = try? retrieve(key: "\(Keys.refreshToken)_\(siteId)")
        let expiresAtInterval = UserDefaults.standard.double(forKey: "\(UserDefaultsKeys.accessExpiresAt)_\(siteId)")
        let expiresAt = expiresAtInterval > 0 ? Date(timeIntervalSince1970: expiresAtInterval) : nil
        
        return (accessToken, refreshToken, expiresAt)
    }
    
    static func clearTokens(for siteId: String) {
        try? delete(key: "\(Keys.accessToken)_\(siteId)")
        try? delete(key: "\(Keys.refreshToken)_\(siteId)")
        UserDefaults.standard.removeObject(forKey: "\(UserDefaultsKeys.accessExpiresAt)_\(siteId)")
    }
    
    // MARK: - Legacy Token Methods (for migration/cleanup)
    
    static func clearLegacyTokens() {
        try? delete(key: Keys.accessToken)
        try? delete(key: Keys.refreshToken)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.accessExpiresAt)
    }
    
    // MARK: - Device ID (UserDefaults - not sensitive)
    
    static func getOrCreateDeviceId() -> String {
        if let existing = UserDefaults.standard.string(forKey: UserDefaultsKeys.deviceId) {
            return existing
        }
        
        let newId = UUID().uuidString
        UserDefaults.standard.set(newId, forKey: UserDefaultsKeys.deviceId)
        return newId
    }
    
    // MARK: - Private Helpers
    
    private static func store(key: String, value: String) throws {
        let data = Data(value.utf8)
        
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlocked
        ]
        
        // Try to delete existing item first
        SecItemDelete(query as CFDictionary)
        
        let status = SecItemAdd(query as CFDictionary, nil)
        
        guard status == errSecSuccess else {
            throw KeychainError.unexpectedStatus(status)
        }
    }
    
    private static func retrieve(key: String) throws -> String {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess else {
            throw KeychainError.itemNotFound
        }
        
        guard let data = result as? Data, let value = String(data: data, encoding: .utf8) else {
            throw KeychainError.invalidData
        }
        
        return value
    }
    
    private static func delete(key: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key
        ]
        
        let status = SecItemDelete(query as CFDictionary)
        
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }
    
    // MARK: - SSH Key Storage (Per-Server)
    
    /// Store SSH key pair for a specific server
    static func storeSSHKeyPair(forServer serverId: String, privateKey: String, publicKey: String) throws {
        try store(key: "\(Keys.sshPrivateKey)_\(serverId)", value: privateKey)
        UserDefaults.standard.set(publicKey, forKey: "\(UserDefaultsKeys.sshPublicKey)_\(serverId)")
    }
    
    /// Get SSH key pair for a specific server
    static func getSSHKeyPair(forServer serverId: String) -> (privateKey: String?, publicKey: String?) {
        let privateKey = try? retrieve(key: "\(Keys.sshPrivateKey)_\(serverId)")
        let publicKey = UserDefaults.standard.string(forKey: "\(UserDefaultsKeys.sshPublicKey)_\(serverId)")
        return (privateKey, publicKey)
    }
    
    /// Check if SSH key exists for a specific server
    static func hasSSHKey(forServer serverId: String) -> Bool {
        let keyPath = sshKeyPath(forServer: serverId)
        return FileManager.default.fileExists(atPath: keyPath)
    }
    
    /// Get the file path for a server's SSH private key
    static func sshKeyPath(forServer serverId: String) -> String {
        AppPaths.key(forServer: serverId).path
    }
    
    /// Get the file path for a server's SSH public key
    static func sshPublicKeyPath(forServer serverId: String) -> String {
        sshKeyPath(forServer: serverId) + ".pub"
    }
    
    /// Clear SSH keys for a specific server
    static func clearSSHKeys(forServer serverId: String) {
        try? delete(key: "\(Keys.sshPrivateKey)_\(serverId)")
        UserDefaults.standard.removeObject(forKey: "\(UserDefaultsKeys.sshPublicKey)_\(serverId)")
        // Also remove key files
        try? FileManager.default.removeItem(atPath: sshKeyPath(forServer: serverId))
        try? FileManager.default.removeItem(atPath: sshPublicKeyPath(forServer: serverId))
    }
    
    // MARK: - Live MySQL Credentials
    
    static func storeLiveMySQLCredentials(username: String, password: String, database: String) throws {
        try store(key: Keys.liveMySQLPassword, value: password)
        UserDefaults.standard.set(username, forKey: UserDefaultsKeys.liveMySQLUsername)
        UserDefaults.standard.set(database, forKey: UserDefaultsKeys.liveMySQLDatabase)
    }
    
    static func getLiveMySQLCredentials() -> (username: String?, password: String?, database: String?) {
        let username = UserDefaults.standard.string(forKey: UserDefaultsKeys.liveMySQLUsername)
        let password = try? retrieve(key: Keys.liveMySQLPassword)
        let database = UserDefaults.standard.string(forKey: UserDefaultsKeys.liveMySQLDatabase)
        return (username, password, database)
    }
    
    static func hasLiveMySQLCredentials() -> Bool {
        let creds = getLiveMySQLCredentials()
        return creds.username != nil && !creds.username!.isEmpty &&
               creds.database != nil && !creds.database!.isEmpty
    }
    
    static func clearLiveMySQLCredentials() {
        try? delete(key: Keys.liveMySQLPassword)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.liveMySQLUsername)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.liveMySQLDatabase)
    }
    
    // MARK: - Reset All
    
    static func clearAll(for siteId: String) {
        clearTokens(for: siteId)
        clearLegacyTokens()
        clearLiveMySQLCredentials()
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.deviceId)
    }
}
