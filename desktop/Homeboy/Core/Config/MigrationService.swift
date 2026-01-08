import Foundation
import Security

/// Handles migration from ExtraChillDesktop to Homeboy.
/// Migrates Application Support folder, Keychain items, and UserDefaults on first launch.
struct MigrationService {
    private static let legacyAppName = "ExtraChillDesktop"
    private static let newAppName = "Homeboy"
    private static let legacyKeychainService = "com.extrachill.desktop"
    private static let newKeychainService = "com.extrachill.homeboy"
    private static let legacyBundleId = "com.extrachill.desktop"
    
    /// Keychain keys that need migration
    private static let keychainKeys = [
        "accessToken",
        "refreshToken",
        "sshPrivateKey",
        "liveMySQLPassword"
    ]
    
    /// UserDefaults keys that need migration
    private static let userDefaultsKeys = [
        "serverHost",
        "serverUsername",
        "serverAppPath",
        "cloudwaysHost",
        "cloudwaysUsername",
        "cloudwaysAppPath",
        "sshPublicKey",
        "deviceId",
        "liveMySQLUsername",
        "liveMySQLDatabase"
    ]
    
    /// Key used to track if UserDefaults migration has been completed
    private static let userDefaultsMigrationKey = "didMigrateFromExtraChillDesktop"
    
    // MARK: - Public API
    
    /// Check if migration is needed and perform it.
    /// Call this before any configuration loading.
    static func migrateIfNeeded() {
        let fileManager = FileManager.default
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        
        let legacyDir = appSupport.appendingPathComponent(legacyAppName)
        let newDir = appSupport.appendingPathComponent(newAppName)
        
        // Only migrate if legacy exists and new doesn't
        let legacyExists = fileManager.fileExists(atPath: legacyDir.path)
        let newExists = fileManager.fileExists(atPath: newDir.path)
        
        if legacyExists && !newExists {
            print("[MigrationService] Migrating from \(legacyAppName) to \(newAppName)...")
            migrateApplicationSupportFolder(from: legacyDir, to: newDir)
            migrateKeychainItems()
            migrateUserDefaults()
            print("[MigrationService] Migration complete")
        } else if legacyExists && newExists {
            // Both exist - just migrate keychain and UserDefaults if needed (idempotent)
            migrateKeychainItems()
            migrateUserDefaults()
        } else {
            // Even if legacy folder doesn't exist, UserDefaults might need migration
            migrateUserDefaults()
        }
    }
    
    // MARK: - Application Support Migration
    
    private static func migrateApplicationSupportFolder(from legacyDir: URL, to newDir: URL) {
        let fileManager = FileManager.default
        
        do {
            // Move entire directory (atomic operation)
            try fileManager.moveItem(at: legacyDir, to: newDir)
            print("[MigrationService] Moved Application Support folder successfully")
        } catch {
            print("[MigrationService] Failed to move folder: \(error.localizedDescription)")
            // Fallback: try to copy instead
            do {
                try fileManager.copyItem(at: legacyDir, to: newDir)
                print("[MigrationService] Copied Application Support folder (fallback)")
            } catch {
                print("[MigrationService] Failed to copy folder: \(error.localizedDescription)")
            }
        }
    }
    
    // MARK: - Keychain Migration
    
    private static func migrateKeychainItems() {
        // First, get all site IDs from config to migrate site-specific tokens
        let siteIds = getSiteIdsFromConfig()
        
        // Migrate base keys
        for key in keychainKeys {
            migrateKeychainItem(key: key)
        }
        
        // Migrate site-specific token keys
        for siteId in siteIds {
            migrateKeychainItem(key: "accessToken_\(siteId)")
            migrateKeychainItem(key: "refreshToken_\(siteId)")
        }
    }
    
    private static func migrateKeychainItem(key: String) {
        // Check if item exists in new service already
        if keychainItemExists(service: newKeychainService, key: key) {
            return // Already migrated
        }
        
        // Try to read from legacy service
        guard let value = readKeychainItem(service: legacyKeychainService, key: key) else {
            return // No legacy item to migrate
        }
        
        // Write to new service
        if writeKeychainItem(service: newKeychainService, key: key, value: value) {
            print("[MigrationService] Migrated keychain item: \(key)")
            // Optionally delete from legacy service
            deleteKeychainItem(service: legacyKeychainService, key: key)
        }
    }
    
    private static func getSiteIdsFromConfig() -> [String] {
        let fileManager = FileManager.default
        let appSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        
        // Check both locations (migration may or may not have happened yet)
        let newSitesDir = appSupport.appendingPathComponent(newAppName).appendingPathComponent("sites")
        let legacySitesDir = appSupport.appendingPathComponent(legacyAppName).appendingPathComponent("sites")
        
        let sitesDir = fileManager.fileExists(atPath: newSitesDir.path) ? newSitesDir : legacySitesDir
        
        guard fileManager.fileExists(atPath: sitesDir.path) else {
            return []
        }
        
        do {
            let files = try fileManager.contentsOfDirectory(at: sitesDir, includingPropertiesForKeys: nil)
            return files
                .filter { $0.pathExtension == "json" }
                .map { $0.deletingPathExtension().lastPathComponent }
        } catch {
            return []
        }
    }
    
    // MARK: - Keychain Helpers
    
    private static func keychainItemExists(service: String, key: String) -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: false
        ]
        
        let status = SecItemCopyMatching(query as CFDictionary, nil)
        return status == errSecSuccess
    }
    
    private static func readKeychainItem(service: String, key: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess,
              let data = result as? Data,
              let value = String(data: data, encoding: .utf8) else {
            return nil
        }
        
        return value
    }
    
    private static func writeKeychainItem(service: String, key: String, value: String) -> Bool {
        let data = Data(value.utf8)
        
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlocked
        ]
        
        // Delete existing item first (if any)
        SecItemDelete(query as CFDictionary)
        
        let status = SecItemAdd(query as CFDictionary, nil)
        return status == errSecSuccess
    }
    
    private static func deleteKeychainItem(service: String, key: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key
        ]
        
        SecItemDelete(query as CFDictionary)
    }
    
    // MARK: - UserDefaults Migration
    
    private static func migrateUserDefaults() {
        // Check if we've already migrated
        if UserDefaults.standard.bool(forKey: userDefaultsMigrationKey) {
            return
        }
        
        // Path to legacy preferences plist
        let homeDir = FileManager.default.homeDirectoryForCurrentUser
        let legacyPlistPath = homeDir
            .appendingPathComponent("Library/Preferences")
            .appendingPathComponent("\(legacyBundleId).plist")
        
        guard FileManager.default.fileExists(atPath: legacyPlistPath.path) else {
            // No legacy plist to migrate from, mark as done
            UserDefaults.standard.set(true, forKey: userDefaultsMigrationKey)
            return
        }
        
        // Load legacy plist
        guard let legacyDefaults = NSDictionary(contentsOf: legacyPlistPath) as? [String: Any] else {
            print("[MigrationService] Failed to read legacy UserDefaults plist")
            UserDefaults.standard.set(true, forKey: userDefaultsMigrationKey)
            return
        }
        
        print("[MigrationService] Migrating UserDefaults from \(legacyBundleId)...")
        
        // Migrate known keys
        for key in userDefaultsKeys {
            if let value = legacyDefaults[key] {
                // Only migrate if we don't already have a value
                if UserDefaults.standard.object(forKey: key) == nil {
                    UserDefaults.standard.set(value, forKey: key)
                    print("[MigrationService] Migrated UserDefaults key: \(key)")
                }
            }
        }
        
        // Migrate any accessExpiresAt_* keys (site-specific token expiry)
        for (key, value) in legacyDefaults {
            if key.hasPrefix("accessExpiresAt_") {
                if UserDefaults.standard.object(forKey: key) == nil {
                    UserDefaults.standard.set(value, forKey: key)
                    print("[MigrationService] Migrated UserDefaults key: \(key)")
                }
            }
        }
        
        // Mark migration as complete
        UserDefaults.standard.set(true, forKey: userDefaultsMigrationKey)
        print("[MigrationService] UserDefaults migration complete")
    }
}
