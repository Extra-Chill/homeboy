import Foundation

actor APIClient {
    static let shared = APIClient()
    
    private let baseURL = "https://extrachill.com/wp-json/extrachill/v1"
    private let refreshBufferSeconds: TimeInterval = 60
    
    private var accessToken: String?
    private var refreshToken: String?
    private var accessExpiresAt: Date?
    private var isRefreshing = false
    private var onAuthFailure: (() -> Void)?
    
    private init() {}
    
    // MARK: - Initialization
    
    func initialize(onAuthFailure: @escaping () -> Void) {
        self.onAuthFailure = onAuthFailure
        let tokens = KeychainService.getTokens()
        self.accessToken = tokens.accessToken
        self.refreshToken = tokens.refreshToken
        self.accessExpiresAt = tokens.expiresAt
    }
    
    func hasTokens() -> Bool {
        accessToken != nil && refreshToken != nil
    }
    
    // MARK: - Auth Methods
    
    func login(identifier: String, password: String) async throws -> LoginResponse {
        let deviceId = KeychainService.getOrCreateDeviceId()
        let request = LoginRequest(identifier: identifier, password: password, deviceId: deviceId)
        
        let response: LoginResponse = try await post("/auth/login", body: request, requiresAuth: false)
        
        let expiresAt = parseExpiryDate(response.accessExpiresAt)
        try KeychainService.storeTokens(
            accessToken: response.accessToken,
            refreshToken: response.refreshToken,
            expiresAt: expiresAt
        )
        
        self.accessToken = response.accessToken
        self.refreshToken = response.refreshToken
        self.accessExpiresAt = expiresAt
        
        return response
    }
    
    func logout() async {
        if accessToken != nil {
            let deviceId = KeychainService.getOrCreateDeviceId()
            let request = LogoutRequest(deviceId: deviceId)
            try? await post("/auth/logout", body: request, requiresAuth: true) as EmptyResponse
        }
        
        clearAuth()
    }
    
    func getMe() async throws -> User {
        try await get("/auth/me")
    }
    
    // MARK: - Newsletter Methods
    
    func bulkSubscribe(emails: [EmailEntry], listId: String, source: String = "bandcamp-scraper") async throws -> BulkSubscribeResponse {
        let request = BulkSubscribeRequest(emails: emails, listId: listId, source: source)
        return try await post("/newsletter/subscribe", body: request, requiresAuth: true)
    }
    
    // MARK: - Private Methods
    
    private func get<T: Decodable>(_ endpoint: String, requiresAuth: Bool = true) async throws -> T {
        try await request(endpoint, method: "GET", body: nil as EmptyBody?, requiresAuth: requiresAuth)
    }
    
    private func post<T: Decodable, B: Encodable>(_ endpoint: String, body: B, requiresAuth: Bool = true) async throws -> T {
        try await request(endpoint, method: "POST", body: body, requiresAuth: requiresAuth)
    }
    
    private func request<T: Decodable, B: Encodable>(
        _ endpoint: String,
        method: String,
        body: B?,
        requiresAuth: Bool
    ) async throws -> T {
        if requiresAuth {
            try await ensureValidToken()
        }
        
        guard let url = URL(string: baseURL + endpoint) else {
            throw URLError(.badURL)
        }
        
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        
        if requiresAuth, let token = accessToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        
        if let body = body {
            request.httpBody = try JSONEncoder().encode(body)
        }
        
        let (data, response) = try await URLSession.shared.data(for: request)
        
        guard let httpResponse = response as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        
        // Handle 401 - try refresh and retry
        if httpResponse.statusCode == 401 && requiresAuth {
            let refreshed = await refreshAccessToken()
            if !refreshed {
                await handleAuthFailure()
                throw APIError(code: "auth_failed", message: "Session expired", data: nil)
            }
            
            // Retry with new token
            var retryRequest = request
            if let token = accessToken {
                retryRequest.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            }
            
            let (retryData, retryResponse) = try await URLSession.shared.data(for: retryRequest)
            guard let retryHttpResponse = retryResponse as? HTTPURLResponse else {
                throw URLError(.badServerResponse)
            }
            
            if retryHttpResponse.statusCode >= 400 {
                let error = try JSONDecoder().decode(APIError.self, from: retryData)
                throw error
            }
            
            return try JSONDecoder().decode(T.self, from: retryData)
        }
        
        if httpResponse.statusCode >= 400 {
            let error = try JSONDecoder().decode(APIError.self, from: data)
            throw error
        }
        
        return try JSONDecoder().decode(T.self, from: data)
    }
    
    private func ensureValidToken() async throws {
        guard accessToken != nil, refreshToken != nil else {
            throw APIError(code: "no_token", message: "Not authenticated", data: nil)
        }
        
        if isAccessExpiringSoon() {
            let refreshed = await refreshAccessToken()
            if !refreshed {
                await handleAuthFailure()
                throw APIError(code: "refresh_failed", message: "Session expired", data: nil)
            }
        }
    }
    
    private func isAccessExpiringSoon() -> Bool {
        guard let expiresAt = accessExpiresAt else { return true }
        return Date().addingTimeInterval(refreshBufferSeconds) >= expiresAt
    }
    
    private func refreshAccessToken() async -> Bool {
        guard !isRefreshing, let currentRefreshToken = refreshToken else {
            return false
        }
        
        isRefreshing = true
        defer { isRefreshing = false }
        
        do {
            let deviceId = KeychainService.getOrCreateDeviceId()
            let request = RefreshRequest(refreshToken: currentRefreshToken, deviceId: deviceId)
            
            let response: RefreshResponse = try await post("/auth/refresh", body: request, requiresAuth: false)
            
            let expiresAt = parseExpiryDate(response.accessExpiresAt)
            try KeychainService.storeTokens(
                accessToken: response.accessToken,
                refreshToken: response.refreshToken,
                expiresAt: expiresAt
            )
            
            self.accessToken = response.accessToken
            self.refreshToken = response.refreshToken
            self.accessExpiresAt = expiresAt
            
            return true
        } catch {
            return false
        }
    }
    
    private func handleAuthFailure() async {
        clearAuth()
        onAuthFailure?()
    }
    
    private func clearAuth() {
        accessToken = nil
        refreshToken = nil
        accessExpiresAt = nil
        KeychainService.clearTokens()
    }
    
    private func parseExpiryDate(_ dateString: String) -> Date {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: dateString) ?? Date().addingTimeInterval(3600)
    }
}

// MARK: - Helper Types

private struct EmptyBody: Encodable {}
private struct EmptyResponse: Decodable {}
