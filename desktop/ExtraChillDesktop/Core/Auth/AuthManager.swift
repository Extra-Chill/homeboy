import Foundation
import SwiftUI

@MainActor
class AuthManager: ObservableObject {
    @Published var user: User?
    @Published var isLoading = true
    @Published var isAuthenticated = false
    @Published var error: String?
    
    init() {
        Task {
            await checkAuth()
        }
    }
    
    func checkAuth() async {
        await APIClient.shared.initialize { [weak self] in
            Task { @MainActor in
                self?.handleAuthFailure()
            }
        }
        
        guard await APIClient.shared.hasTokens() else {
            isLoading = false
            return
        }
        
        do {
            let user = try await APIClient.shared.getMe()
            self.user = user
            self.isAuthenticated = true
        } catch {
            // Token invalid, clear state
            self.user = nil
            self.isAuthenticated = false
        }
        
        isLoading = false
    }
    
    func login(identifier: String, password: String) async {
        error = nil
        isLoading = true
        
        do {
            let response = try await APIClient.shared.login(identifier: identifier, password: password)
            self.user = response.user
            self.isAuthenticated = true
        } catch let apiError as APIError {
            self.error = apiError.message
        } catch {
            self.error = error.localizedDescription
        }
        
        isLoading = false
    }
    
    func logout() {
        Task {
            await APIClient.shared.logout()
            self.user = nil
            self.isAuthenticated = false
        }
    }
    
    private func handleAuthFailure() {
        user = nil
        isAuthenticated = false
    }
}
