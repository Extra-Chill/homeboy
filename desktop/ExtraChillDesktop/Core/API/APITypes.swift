import Foundation

// MARK: - Auth Types

struct LoginRequest: Codable {
    let identifier: String
    let password: String
    let deviceId: String
    
    enum CodingKeys: String, CodingKey {
        case identifier
        case password
        case deviceId = "device_id"
    }
}

struct LoginResponse: Codable {
    let accessToken: String
    let refreshToken: String
    let accessExpiresAt: String
    let user: User
    
    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case accessExpiresAt = "access_expires_at"
        case user
    }
}

struct RefreshRequest: Codable {
    let refreshToken: String
    let deviceId: String
    
    enum CodingKeys: String, CodingKey {
        case refreshToken = "refresh_token"
        case deviceId = "device_id"
    }
}

struct RefreshResponse: Codable {
    let accessToken: String
    let refreshToken: String
    let accessExpiresAt: String
    
    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case accessExpiresAt = "access_expires_at"
    }
}

struct User: Codable {
    let id: Int
    let username: String
    let email: String
    let displayName: String
    let profileUrl: String?
    
    enum CodingKeys: String, CodingKey {
        case id
        case username
        case email
        case displayName = "display_name"
        case profileUrl = "profile_url"
    }
}

struct LogoutRequest: Codable {
    let deviceId: String
    
    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
    }
}

// MARK: - Newsletter Types

struct BulkSubscribeRequest: Codable {
    let emails: [EmailEntry]
    let listId: String
    
    enum CodingKeys: String, CodingKey {
        case emails
        case listId = "list_id"
    }
}

struct EmailEntry: Codable {
    let email: String
    let name: String
}

struct BulkSubscribeResponse: Codable {
    let success: Bool
    let subscribed: Int
    let alreadySubscribed: Int
    let failed: Int
    let errors: [String]
    
    enum CodingKeys: String, CodingKey {
        case success
        case subscribed
        case alreadySubscribed = "already_subscribed"
        case failed
        case errors
    }
}

// MARK: - API Error

struct APIError: Codable, Error {
    let code: String?
    let message: String
    let data: APIErrorData?
}

struct APIErrorData: Codable {
    let status: Int?
}
