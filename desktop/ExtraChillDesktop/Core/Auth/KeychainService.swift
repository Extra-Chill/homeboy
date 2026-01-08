import Foundation
import Security

enum KeychainError: Error {
    case duplicateItem
    case itemNotFound
    case unexpectedStatus(OSStatus)
    case invalidData
}

struct KeychainService {
    private static let service = "com.extrachill.desktop"
    
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
        static let cloudwaysHost = "cloudwaysHost"
        static let cloudwaysUsername = "cloudwaysUsername"
        static let cloudwaysAppPath = "cloudwaysAppPath"
        static let sshPublicKey = "sshPublicKey"
        // MySQL credentials (non-sensitive parts)
        static let liveMySQLUsername = "liveMySQLUsername"
        static let liveMySQLDatabase = "liveMySQLDatabase"
    }
    
    // MARK: - Token Storage
    
    static func storeTokens(accessToken: String, refreshToken: String, expiresAt: Date) throws {
        try store(key: Keys.accessToken, value: accessToken)
        try store(key: Keys.refreshToken, value: refreshToken)
        UserDefaults.standard.set(expiresAt.timeIntervalSince1970, forKey: UserDefaultsKeys.accessExpiresAt)
    }
    
    static func getTokens() -> (accessToken: String?, refreshToken: String?, expiresAt: Date?) {
        let accessToken = try? retrieve(key: Keys.accessToken)
        let refreshToken = try? retrieve(key: Keys.refreshToken)
        let expiresAtInterval = UserDefaults.standard.double(forKey: UserDefaultsKeys.accessExpiresAt)
        let expiresAt = expiresAtInterval > 0 ? Date(timeIntervalSince1970: expiresAtInterval) : nil
        
        return (accessToken, refreshToken, expiresAt)
    }
    
    static func clearTokens() {
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
    
    // MARK: - Cloudways Credentials (UserDefaults - not sensitive)
    
    static func storeCloudwaysCredentials(host: String, username: String, appPath: String) {
        UserDefaults.standard.set(host, forKey: UserDefaultsKeys.cloudwaysHost)
        UserDefaults.standard.set(username, forKey: UserDefaultsKeys.cloudwaysUsername)
        UserDefaults.standard.set(appPath, forKey: UserDefaultsKeys.cloudwaysAppPath)
    }
    
    static func getCloudwaysCredentials() -> (host: String?, username: String?, appPath: String?) {
        let host = UserDefaults.standard.string(forKey: UserDefaultsKeys.cloudwaysHost)
        let username = UserDefaults.standard.string(forKey: UserDefaultsKeys.cloudwaysUsername)
        let appPath = UserDefaults.standard.string(forKey: UserDefaultsKeys.cloudwaysAppPath)
        return (host, username, appPath)
    }
    
    static func hasCloudwaysCredentials() -> Bool {
        let creds = getCloudwaysCredentials()
        return creds.host != nil && !creds.host!.isEmpty &&
               creds.username != nil && !creds.username!.isEmpty &&
               creds.appPath != nil && !creds.appPath!.isEmpty
    }
    
    static func clearCloudwaysCredentials() {
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.cloudwaysHost)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.cloudwaysUsername)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.cloudwaysAppPath)
    }
    
    // MARK: - SSH Key Storage (Private key in Keychain, public key in UserDefaults)
    
    static func storeSSHKeyPair(privateKey: String, publicKey: String) throws {
        try store(key: Keys.sshPrivateKey, value: privateKey)
        UserDefaults.standard.set(publicKey, forKey: UserDefaultsKeys.sshPublicKey)
    }
    
    static func getSSHKeyPair() -> (privateKey: String?, publicKey: String?) {
        let privateKey = try? retrieve(key: Keys.sshPrivateKey)
        let publicKey = UserDefaults.standard.string(forKey: UserDefaultsKeys.sshPublicKey)
        return (privateKey, publicKey)
    }
    
    static func hasSSHKey() -> Bool {
        // Check file on disk instead of Keychain to avoid password prompts
        let keyPath = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("ExtraChillDesktop")
            .appendingPathComponent("id_rsa")
            .path
        return FileManager.default.fileExists(atPath: keyPath)
    }
    
    static func clearSSHKeys() {
        try? delete(key: Keys.sshPrivateKey)
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.sshPublicKey)
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
    
    static func clearAll() {
        clearTokens()
        clearCloudwaysCredentials()
        clearSSHKeys()
        clearLiveMySQLCredentials()
        UserDefaults.standard.removeObject(forKey: UserDefaultsKeys.deviceId)
    }
}
