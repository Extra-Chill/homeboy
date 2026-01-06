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
        static let accessExpiresAt = "accessExpiresAt"
        static let deviceId = "deviceId"
    }
    
    // MARK: - Token Storage
    
    static func storeTokens(accessToken: String, refreshToken: String, expiresAt: Date) throws {
        try store(key: Keys.accessToken, value: accessToken)
        try store(key: Keys.refreshToken, value: refreshToken)
        try store(key: Keys.accessExpiresAt, value: String(expiresAt.timeIntervalSince1970))
    }
    
    static func getTokens() -> (accessToken: String?, refreshToken: String?, expiresAt: Date?) {
        let accessToken = try? retrieve(key: Keys.accessToken)
        let refreshToken = try? retrieve(key: Keys.refreshToken)
        let expiresAtString = try? retrieve(key: Keys.accessExpiresAt)
        let expiresAt = expiresAtString.flatMap { Double($0) }.map { Date(timeIntervalSince1970: $0) }
        
        return (accessToken, refreshToken, expiresAt)
    }
    
    static func clearTokens() {
        try? delete(key: Keys.accessToken)
        try? delete(key: Keys.refreshToken)
        try? delete(key: Keys.accessExpiresAt)
    }
    
    // MARK: - Device ID
    
    static func getOrCreateDeviceId() -> String {
        if let existing = try? retrieve(key: Keys.deviceId) {
            return existing
        }
        
        let newId = UUID().uuidString
        try? store(key: Keys.deviceId, value: newId)
        return newId
    }
    
    // MARK: - Private Helpers
    
    private static func store(key: String, value: String) throws {
        let data = Data(value.utf8)
        
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecValueData as String: data
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
}
