import CryptoKit
import Foundation

/// Read-and-act client for Homeboy core's `/v1/control-plane/*` HTTP surface.
///
/// Homeboy core owns execution, lifecycle, policy, and mutations. This client
/// only decodes the typed resources core returns and posts actions core has
/// already reported as available via `action_eligibility`; it never invents
/// a mutation core has not exposed.
actor ControlPlaneClient {
    static let shared = ControlPlaneClient()

    private let locator: HomeboyDaemonLocator
    private let session: URLSession
    private let decoder: JSONDecoder

    init(locator: HomeboyDaemonLocator = .shared, session: URLSession = .shared) {
        self.locator = locator
        self.session = session
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.decoder = decoder
    }

    // MARK: - Reads

    func capabilities() async throws -> ControlPlaneCapabilities {
        try await get("/v1/control-plane/capabilities")
    }

    func missions(limit: Int? = nil, cursor: String? = nil) async throws -> ControlPlaneMissionPage {
        var query: [String: String] = [:]
        if let limit { query["limit"] = String(limit) }
        if let cursor { query["cursor"] = cursor }
        return try await get("/v1/control-plane/missions", query: query)
    }

    func runs(limit: Int? = nil, cursor: String? = nil, mission: String? = nil) async throws -> ControlPlaneRunPage {
        var query: [String: String] = [:]
        if let limit { query["limit"] = String(limit) }
        if let cursor { query["cursor"] = cursor }
        if let mission { query["mission"] = mission }
        return try await get("/v1/control-plane/runs", query: query)
    }

    func run(id: String) async throws -> ControlPlaneRun {
        try await get("/v1/control-plane/runs/\(pathEscape(id))")
    }

    /// Fetches events after `cursor`. Throws a `ControlPlaneError` with
    /// `isCursorExpired == true` on HTTP 410 so `MissionStore` can re-snapshot
    /// from the beginning of the retained window.
    func events(runId: String, cursor: String? = nil) async throws -> ControlPlaneEventPage {
        var query: [String: String] = [:]
        if let cursor { query["cursor"] = cursor }
        return try await get("/v1/control-plane/runs/\(pathEscape(runId))/events", query: query)
    }

    func review(runId: String) async throws -> ControlPlaneRunReview {
        try await get("/v1/control-plane/runs/\(pathEscape(runId))/review")
    }

    // MARK: - Actions

    /// Posts an action Homeboy core has already reported as available for
    /// this run. `parameters` must match the payload schema core expects for
    /// `action` (see `ControlPlaneActionParameters`); callers build it from
    /// the `required_inputs` on the corresponding `ControlPlaneRunAction`.
    @discardableResult
    func performAction(
        runId: String,
        action: String,
        parameters: ControlPlaneActionParameters,
        idempotencyKey: String = UUID().uuidString,
        actor: String = "homeboy-desktop"
    ) async throws -> ControlPlaneRun {
        let effectID = Self.actionEffectID(
            provenance: actor,
            targetID: runId,
            action: action,
            idempotencyKey: idempotencyKey
        )

        let body: [String: Any] = [
            "schema": "homeboy/control-plane-action-request/v1",
            "effect_id": effectID,
            "action": action,
            "idempotency_key": idempotencyKey,
            "actor": actor,
            "parameters": parameters.wireObject,
            "confirmed": true,
        ]

        let acknowledgement: ControlPlaneActionAcknowledgement = try await post(
            "/v1/control-plane/runs/\(pathEscape(runId))/actions",
            jsonBody: body
        )
        return acknowledgement.resource
    }

    /// Builds the same deterministic effect identity Homeboy's CLI computes
    /// (`action_effect_id` in `homeboy-control-plane-contract`): a short,
    /// human-legible form when it fits the 128-byte bound, otherwise a
    /// collision-resistant digest that still starts with `provenance:action`.
    static func actionEffectID(provenance: String, targetID: String, action: String, idempotencyKey: String) -> String {
        let legacy = "\(provenance):\(targetID):\(action):\(idempotencyKey)"
        if legacy.utf8.count <= 128 {
            return legacy
        }

        var digestInput = Data()
        for component in [provenance, targetID, action, idempotencyKey] {
            var length = UInt64(component.utf8.count).bigEndian
            withUnsafeBytes(of: &length) { digestInput.append(contentsOf: $0) }
            digestInput.append(contentsOf: component.utf8)
        }
        let digest = SHA256.hash(data: digestInput)
        let hex = digest.map { String(format: "%02x", $0) }.joined()
        return "\(provenance):\(action):\(hex)"
    }

    // MARK: - Transport

    private func get<T: Decodable>(_ path: String, query: [String: String] = [:]) async throws -> T {
        let baseURL = try await resolvedBaseURL()
        var components = URLComponents(url: baseURL.appendingPathComponent(path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))), resolvingAgainstBaseURL: false)
        if !query.isEmpty {
            components?.queryItems = query.map { URLQueryItem(name: $0.key, value: $0.value) }
        }
        guard let url = components?.url else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return try await perform(request, runId: runIdHint(fromPath: path))
    }

    private func post<T: Decodable>(_ path: String, jsonBody: [String: Any]) async throws -> T {
        let baseURL = try await resolvedBaseURL()
        let url = baseURL.appendingPathComponent(path.trimmingCharacters(in: CharacterSet(charactersIn: "/")))

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = try JSONSerialization.data(withJSONObject: jsonBody)
        return try await perform(request, runId: runIdHint(fromPath: path))
    }

    private func perform<T: Decodable>(_ request: URLRequest, runId: String?) async throws -> T {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            // A transport failure (connection refused, timed out) most often
            // means the daemon restarted on a new port. Force rediscovery on
            // the next call rather than retrying against a stale address.
            await locator.invalidate()
            throw ControlPlaneError.transport(error)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw ControlPlaneError.transport(URLError(.badServerResponse))
        }

        guard (200..<500).contains(httpResponse.statusCode) else {
            await locator.invalidate()
            throw ControlPlaneError.transport(URLError(.badServerResponse))
        }

        let envelope = try decoder.decode(ControlPlaneEnvelope<T>.self, from: data)

        guard envelope.success, envelope.data.body.ok, let resource = envelope.data.body.resource else {
            if let error = envelope.data.body.error {
                if error.errorClass == "cursor_expired" || httpResponse.statusCode == 410, let runId {
                    throw ControlPlaneError.cursorExpired(runId: runId)
                }
                throw ControlPlaneError(
                    errorClass: error.errorClass ?? "unknown",
                    message: error.message,
                    retryable: error.retryable ?? false,
                    httpStatus: httpResponse.statusCode
                )
            }
            throw ControlPlaneError(
                errorClass: "unknown",
                message: "Control-plane request failed without a structured error.",
                retryable: false,
                httpStatus: httpResponse.statusCode
            )
        }

        return resource
    }

    private func resolvedBaseURL() async throws -> URL {
        do {
            return try await locator.baseURL()
        } catch {
            throw error
        }
    }

    private func pathEscape(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? value
    }

    private func runIdHint(fromPath path: String) -> String? {
        let segments = path.split(separator: "/")
        guard let runsIndex = segments.firstIndex(of: "runs"), segments.count > runsIndex + 1 else {
            return nil
        }
        return String(segments[runsIndex + 1])
    }
}

/// Typed acknowledgement returned by `POST /v1/control-plane/runs/:id/actions`.
private struct ControlPlaneActionAcknowledgement: Decodable {
    let resource: ControlPlaneRun
}

/// Payload sent as `parameters` in a control-plane action request. Each case
/// mirrors the versioned parameter schema Homeboy core validates against
/// (`homeboy-control-plane-contract::action`).
enum ControlPlaneActionParameters {
    case empty
    case cancel(reason: String?)
    case retry(force: Bool)
    case quarantine(reason: String)
    case placementUpdate(placement: String)
    /// Generic fallback for actions whose full payload shape (e.g. `promote`)
    /// is not yet modeled client-side. Callers supply the `required_inputs`
    /// Homeboy core reported as a flat string dictionary.
    case raw(schema: String, data: [String: String])

    var wireObject: [String: Any] {
        switch self {
        case .empty:
            return ["schema": "homeboy/control-plane-empty-action-payload/v1"]
        case .cancel(let reason):
            var data: [String: Any] = [:]
            if let reason, !reason.isEmpty { data["reason"] = reason }
            return [
                "schema": "homeboy/control-plane-cancel-parameters/v1",
                "data": data,
            ]
        case .retry(let force):
            return [
                "schema": "homeboy/control-plane-retry-parameters/v1",
                "data": ["force": force],
            ]
        case .quarantine(let reason):
            return [
                "schema": "homeboy/control-plane-quarantine-parameters/v1",
                "data": ["reason": reason],
            ]
        case .placementUpdate(let placement):
            return [
                "schema": "homeboy/control-plane-placement-update-parameters/v1",
                "data": ["placement": placement],
            ]
        case .raw(let schema, let data):
            return ["schema": schema, "data": data]
        }
    }
}
