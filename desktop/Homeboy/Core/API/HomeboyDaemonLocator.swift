import Foundation

/// Discovers the local Homeboy daemon's HTTP address by shelling out to
/// `homeboy daemon ensure-running` through `CLIBridge`. That core command
/// returns the live daemon, or starts one when none is running, so a daemon
/// stopped by an upgrade or an operator is recovered on the next read instead
/// of leaving the control plane empty. Homeboy core owns the lifecycle and
/// its safety checks; this locator only reads `data.address` and caches it
/// until a connection failure forces rediscovery.
actor HomeboyDaemonLocator {
    static let shared = HomeboyDaemonLocator()

    private let cli: CLIBridge
    private var cachedAddress: String?

    init(cli: CLIBridge = .shared) {
        self.cli = cli
    }

    /// Returns the cached daemon base URL, discovering it first if needed.
    func baseURL() async throws -> URL {
        if let cachedAddress, let url = url(forAddress: cachedAddress) {
            return url
        }
        return try await rediscover()
    }

    /// Forces a fresh `homeboy daemon ensure-running` and updates the cache.
    @discardableResult
    func rediscover() async throws -> URL {
        let output: DaemonEnsureRunningOutput = try await cli.executeCommand(
            HomeboyCommandRegistry.arguments("daemon ensure-running"),
            dataType: DaemonEnsureRunningOutput.self,
            source: "Control Plane",
            timeout: 30
        )

        guard let address = output.address, !address.isEmpty else {
            throw HomeboyDaemonLocatorError.addressUnavailable
        }

        guard let url = url(forAddress: address) else {
            throw HomeboyDaemonLocatorError.invalidAddress(address)
        }

        cachedAddress = address
        return url
    }

    /// Clears the cached address so the next `baseURL()` call rediscovers it.
    /// Callers should invoke this after a connection failure against the
    /// cached address.
    func invalidate() {
        cachedAddress = nil
    }

    private func url(forAddress address: String) -> URL? {
        URL(string: "http://\(address)")
    }
}

enum HomeboyDaemonLocatorError: LocalizedError {
    case addressUnavailable
    case invalidAddress(String)

    var errorDescription: String? {
        switch self {
        case .addressUnavailable:
            return "Homeboy daemon ensure-running did not report a listening address. Run `homeboy daemon status` to check the daemon."
        case .invalidAddress(let address):
            return "Homeboy daemon reported an unusable address: \(address)"
        }
    }
}

/// Mirrors the `homeboy daemon ensure-running` result data. Only the field
/// the locator needs is declared; unknown fields are ignored.
struct DaemonEnsureRunningOutput: Decodable {
    let address: String?
}
