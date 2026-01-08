import SwiftUI

struct LoginView: View {
    @EnvironmentObject var authManager: AuthManager
    @State private var identifier = ""
    @State private var password = ""
    
    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            
            // Logo
            Image("Logo")
                .resizable()
                .scaledToFit()
                .frame(width: 150, height: 150)
            
            Text("Sign In")
                .font(.largeTitle)
                .fontWeight(.bold)
            
            Text("Enter your credentials to continue")
                .foregroundColor(.secondary)
            
            VStack(spacing: 12) {
                TextField("Username or email", text: $identifier)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.username)
                    .disabled(authManager.isLoading)
                
                SecureField("Password", text: $password)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.password)
                    .disabled(authManager.isLoading)
                    .onSubmit {
                        login()
                    }
            }
            .frame(maxWidth: 300)
            
            if let error = authManager.error {
                Text(error)
                    .foregroundColor(.red)
                    .font(.caption)
                    .multilineTextAlignment(.center)
            }
            
            Button(action: login) {
                if authManager.isLoading {
                    ProgressView()
                        .controlSize(.small)
                } else {
                    Text("Sign In")
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(identifier.isEmpty || password.isEmpty || authManager.isLoading)
            .keyboardShortcut(.return, modifiers: [])
            
            Spacer()
        }
        .padding(40)
        .frame(minWidth: 400, minHeight: 500)
    }
    
    private func login() {
        guard !identifier.isEmpty, !password.isEmpty else { return }
        Task {
            await authManager.login(identifier: identifier, password: password)
        }
    }
}

#Preview {
    LoginView()
        .environmentObject(AuthManager())
}
