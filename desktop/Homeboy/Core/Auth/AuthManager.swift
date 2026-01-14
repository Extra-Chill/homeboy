import Foundation
import SwiftUI

@MainActor
class AuthManager: ObservableObject {
    @Published var user: User?
    @Published var isLoading = true
    @Published var isAuthenticated = false
    @Published var error: (any DisplayableError)?
    
    /// Returns true if API authentication is configured for the current project
    var isAuthConfigured: Bool {
        let config = ConfigurationManager.shared.safeActiveProject.api
        return config.enabled && !config.baseURL.isEmpty
    }
    
    init() {
        Task {
            await checkAuth()
        }
    }
    
    func checkAuth() async {
        // Skip auth check if not configured
        guard isAuthConfigured else {
            isLoading = false
            isAuthenticated = false
            return
        }
        
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
            self.error = AppError(apiError.message, source: "Authentication")
        } catch {
            self.error = error.toDisplayableError(source: "Authentication")
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
    
    // MARK: - Project Switching
    
    /// Resets auth state and re-checks authentication for the new active project.
    /// Call this when the active project changes.
    func resetForProjectSwitch() async {
        user = nil
        isAuthenticated = false
        isLoading = true
        error = nil
        
        await APIClient.shared.resetForProjectSwitch()
        await checkAuth()
    }
}
