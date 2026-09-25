import Foundation

/// Discovers the local Homeboy daemon's HTTP address by shelling out to
/// `homeboy daemon status` through `CLIBridge`. Homeboy core owns the daemon
/// lifecycle; this locator only reads `data.daemon.address` and caches it
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

    /// Forces a fresh `homeboy daemon status` read and updates the cache.
    @discardableResult
    func rediscover() async throws -> URL {
        let output: DaemonStatusOutput = try await cli.executeCommand(
            ["daemon", "status"],
            dataType: DaemonStatusOutput.self,
            source: "Control Plane",
            timeout: 15
        )

        guard let address = output.daemon?.address, !address.isEmpty else {
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
            return "Homeboy daemon status did not report a listening address. Run `homeboy daemon status` to check the daemon."
        case .invalidAddress(let address):
            return "Homeboy daemon reported an unusable address: \(address)"
        }
    }
}

/// Mirrors the bounded `homeboy daemon status` projection (`homeboy/daemon-status/v1`).
/// Only the fields the locator needs are declared; unknown fields are ignored.
struct DaemonStatusOutput: Decodable {
    let daemon: DaemonStatusAddress?
}

struct DaemonStatusAddress: Decodable {
    let address: String?
}
