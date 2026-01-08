import Foundation

/// Manages SSH tunnel lifecycle for live database connections.
/// Creates a tunnel forwarding local port to remote MySQL port via Cloudways SSH.
class SSHTunnelService: ObservableObject, @unchecked Sendable {
    
    /// Local port for the tunnel (connects to this locally, forwards to remote MySQL)
    static let localPort: Int = 3307
    
    /// Remote MySQL port on Cloudways server
    private static let remotePort: Int = 3306
    
    /// The running SSH tunnel process
    private var tunnelProcess: Process?
    
    /// Whether the tunnel is currently active
    @Published private(set) var isConnected = false
    
    /// SSH credentials from Cloudways settings
    private let host: String
    private let username: String
    private let privateKeyPath: String
    
    init?() {
        let creds = KeychainService.getCloudwaysCredentials()
        guard let host = creds.host, !host.isEmpty,
              let username = creds.username, !username.isEmpty else {
            return nil
        }
        
        self.host = host
        self.username = username
        self.privateKeyPath = SSHService.defaultKeyPath
    }
    
    /// Starts the SSH tunnel
    /// - Returns: Result indicating success or error
    func connect() async -> Result<Void, MySQLError> {
        // Ensure SSH key exists on disk
        guard SSHService.ensureKeyFileExists() else {
            return .failure(.tunnelFailed("SSH key not configured"))
        }
        
        // Kill any stale tunnel on our port
        if let error = killStaleTunnel() {
            return .failure(error)
        }
        
        // Kill any existing tunnel from this instance
        disconnect()
        
        return await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async { [self] in
                let process = Process()
                process.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
                process.arguments = [
                    "-i", privateKeyPath,
                    "-o", "StrictHostKeyChecking=no",
                    "-o", "BatchMode=yes",
                    "-o", "ExitOnForwardFailure=yes",
                    "-o", "ServerAliveInterval=60",
                    "-N",  // Don't execute remote command
                    "-L", "\(SSHTunnelService.localPort):127.0.0.1:\(SSHTunnelService.remotePort)",
                    "\(username)@\(host)"
                ]
                
                let errorPipe = Pipe()
                process.standardError = errorPipe
                process.standardOutput = FileHandle.nullDevice
                
                do {
                    try process.run()
                    self.tunnelProcess = process
                    
                    // Wait for tunnel to establish (SSH needs time to negotiate)
                    Thread.sleep(forTimeInterval: 3.0)
                    
                    // Check if process is still running (tunnel established)
                    if process.isRunning {
                        DispatchQueue.main.async {
                            self.isConnected = true
                        }
                        continuation.resume(returning: .success(()))
                    } else {
                        let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
                        let errorOutput = String(data: errorData, encoding: .utf8) ?? "Unknown error"
                        continuation.resume(returning: .failure(.tunnelFailed(errorOutput.trimmingCharacters(in: .whitespacesAndNewlines))))
                    }
                } catch {
                    continuation.resume(returning: .failure(.tunnelFailed(error.localizedDescription)))
                }
            }
        }
    }
    
    /// Closes the SSH tunnel
    func disconnect() {
        if let process = tunnelProcess, process.isRunning {
            process.terminate()
        }
        tunnelProcess = nil
        DispatchQueue.main.async { [weak self] in
            self?.isConnected = false
        }
    }
    
    /// Kills any stale SSH tunnel process on our port from a previous session.
    /// Returns nil if port is free or stale tunnel was killed, error if port is in use by non-SSH process.
    private func killStaleTunnel() -> MySQLError? {
        // Get PID of process using our port
        let lsofProcess = Process()
        lsofProcess.executableURL = URL(fileURLWithPath: "/usr/sbin/lsof")
        lsofProcess.arguments = ["-ti:\(SSHTunnelService.localPort)"]
        
        let lsofPipe = Pipe()
        lsofProcess.standardOutput = lsofPipe
        lsofProcess.standardError = FileHandle.nullDevice
        
        do {
            try lsofProcess.run()
            lsofProcess.waitUntilExit()
        } catch {
            return nil // lsof failed, assume port is free
        }
        
        let pidData = lsofPipe.fileHandleForReading.readDataToEndOfFile()
        let pidString = String(data: pidData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        
        // If no PID, port is free
        guard !pidString.isEmpty, let pid = Int(pidString.components(separatedBy: .newlines).first ?? "") else {
            return nil
        }
        
        // Kill whatever is using our port (3307 is not a standard port, safe to claim it)
        let killProcess = Process()
        killProcess.executableURL = URL(fileURLWithPath: "/bin/kill")
        killProcess.arguments = ["-9", "\(pid)"]
        killProcess.standardOutput = FileHandle.nullDevice
        killProcess.standardError = FileHandle.nullDevice
        
        do {
            try killProcess.run()
            killProcess.waitUntilExit()
            // Wait briefly for port to free up
            Thread.sleep(forTimeInterval: 0.5)
        } catch {
            // Kill failed, but try to proceed anyway
        }
        return nil
    }
    
    /// Clean up when service is deallocated
    deinit {
        // Terminate process directly without dispatching to main queue
        // (dispatching would create a dangling reference since we're being deallocated)
        if let process = tunnelProcess, process.isRunning {
            process.terminate()
        }
    }
}
