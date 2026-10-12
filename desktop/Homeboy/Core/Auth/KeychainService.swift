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
        static let sshPrivateKey = "sshPrivateKey"
    }
    
    // Non-sensitive data stored in UserDefaults to avoid Keychain prompts
    private enum UserDefaultsKeys {
        static let sshPublicKey = "sshPublicKey"
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
    
    /// Clear SSH keys for a specific server (Keychain + UserDefaults + disk files)
    static func clearSSHKeys(forServer serverId: String) {
        try? delete(key: "\(Keys.sshPrivateKey)_\(serverId)")
        UserDefaults.standard.removeObject(forKey: "\(UserDefaultsKeys.sshPublicKey)_\(serverId)")
        SSHKeyManager.deleteKeyFiles(forServer: serverId)
    }
}
