import Foundation

// MARK: - Envelope
//
// Every `/v1/control-plane/*` response is wrapped as:
//   { "success": bool, "data": { "body": { "ok": bool, "resource": {...}, "error": {...} }, "endpoint": ..., "status": ... } }
//
// `resource` is present when `ok == true`; `error` is present when `ok == false`.
// State/kind/phase-style fields are decoded as raw `String` everywhere so an
// unrecognized value from a newer Homeboy core build never fails decoding.

struct ControlPlaneEnvelope<Resource: Decodable>: Decodable {
    let success: Bool
    let data: ControlPlaneEnvelopeData<Resource>
}

struct ControlPlaneEnvelopeData<Resource: Decodable>: Decodable {
    let body: ControlPlaneBody<Resource>
    let endpoint: String?
    let status: Int?
}

struct ControlPlaneBody<Resource: Decodable>: Decodable {
    let ok: Bool
    let resource: Resource?
    let error: ControlPlaneErrorPayload?
}

struct ControlPlaneErrorPayload: Decodable {
    let errorClass: String?
    let message: String
    let retryable: Bool?

    private enum CodingKeys: String, CodingKey {
        case errorClass = "class"
        case message
        case retryable
    }
}

/// Typed error surfaced to callers, built from `data.body.error` on failure
/// (or synthesized locally for transport failures like an expired cursor).
struct ControlPlaneError: LocalizedError {
    let errorClass: String
    let message: String
    let retryable: Bool
    var httpStatus: Int?

    var errorDescription: String? { message }

    var isCursorExpired: Bool {
        errorClass == "cursor_expired" || httpStatus == 410
    }

    static func cursorExpired(runId: String) -> ControlPlaneError {
        ControlPlaneError(
            errorClass: "cursor_expired",
            message: "Event cursor for run \(runId) expired; re-snapshotting from the start.",
            retryable: true,
            httpStatus: 410
        )
    }

    static func transport(_ underlying: Error) -> ControlPlaneError {
        ControlPlaneError(
            errorClass: "transport",
            message: underlying.localizedDescription,
            retryable: true,
            httpStatus: nil
        )
    }
}

// MARK: - Capabilities

struct ControlPlaneCapabilities: Decodable {
    let schema: String?
    let resources: [String]
    let operations: [String]
}

// MARK: - Missions

struct ControlPlaneMission: Decodable, Identifiable, Hashable {
    let mission: String
    let createdAt: String?
    let updatedAt: String?
    let runCount: Int?

    var id: String { mission }
}

struct ControlPlaneMissionPage: Decodable {
    let missions: [ControlPlaneMission]
    let nextCursor: String?
    let hasMore: Bool
}

// MARK: - Runs

struct ControlPlaneRunPage: Decodable {
    let runs: [ControlPlaneRun]
    let nextCursor: String?
    let hasMore: Bool
}

struct ControlPlaneRun: Decodable, Identifiable, Hashable {
    let run: String
    let mission: String?
    let state: String
    let phase: String?
    let blocker: ControlPlaneBlocker?
    let candidate: ControlPlaneStateSummary?
    let placement: ControlPlanePlacement?
    let provider: ControlPlaneProvider?
    let owner: ControlPlaneOwner?
    let createdAt: String?
    let updatedAt: String?
    let finishedAt: String?
    let heartbeatAt: String?
    let artifacts: [ControlPlaneEvidenceRef]?
    let evidence: [ControlPlaneEvidenceRef]?
    let actionEligibility: ControlPlaneActionEligibilityReport?

    var id: String { run }

    /// Runs stop advancing once they report a completion timestamp. State
    /// strings are open-ended, so `finished_at` is the one reliable signal.
    var isTerminal: Bool { finishedAt != nil }

    static func == (lhs: ControlPlaneRun, rhs: ControlPlaneRun) -> Bool { lhs.run == rhs.run }
    func hash(into hasher: inout Hasher) { hasher.combine(run) }
}

struct ControlPlaneBlocker: Decodable, Hashable {
    let code: String?
    let message: String
    let state: String?
    let reason: String?
}

struct ControlPlaneStateSummary: Decodable, Hashable {
    let id: String?
    let state: String
}

struct ControlPlanePlacement: Decodable, Hashable {
    let decisionId: String?
    let requested: String?
    let selected: String?
    let runnerId: String?
}

struct ControlPlaneProvider: Decodable, Hashable {
    let id: String
    let state: String?
}

struct ControlPlaneOwner: Decodable, Hashable {
    let kind: String
    let id: String
}

struct ControlPlaneEvidenceRef: Decodable, Identifiable, Hashable {
    let id: String
    let kind: String
    let uri: String
}

// MARK: - Action Eligibility

struct ControlPlaneActionEligibilityReport: Decodable, Hashable {
    let run: String?
    let actions: [ControlPlaneRunAction]
}

struct ControlPlaneRunAction: Decodable, Identifiable, Hashable {
    let action: String
    let availability: String
    let reason: String
    let confirmation: String
    let requiredInputs: [String]?
    let idempotent: Bool?
    let requiresRevalidation: Bool?
    let resultResourceType: String?

    var id: String { action }
    var isAvailable: Bool { availability == "available" }
    var requiresConfirmation: Bool { confirmation == "required" }

    /// Title-cased label for the button, e.g. "placement_update" -> "Placement Update".
    var displayName: String {
        action
            .split(separator: "_")
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}

// MARK: - Events

struct ControlPlaneEventPage: Decodable {
    let run: String?
    let events: [ControlPlaneEvent]
    let nextCursor: String?
    let hasMore: Bool
}

struct ControlPlaneEvent: Decodable, Identifiable, Hashable {
    let event: String
    let sequence: Int
    let occurredAt: String?
    let run: String?
    let task: String?
    let kind: String
    let source: ControlPlaneEventSource?
    let data: JSONValue?

    var id: String { event }

    static func == (lhs: ControlPlaneEvent, rhs: ControlPlaneEvent) -> Bool { lhs.event == rhs.event }
    func hash(into hasher: inout Hasher) { hasher.combine(event) }
}

struct ControlPlaneEventSource: Decodable, Hashable {
    let component: String
    let instance: String?
}

extension JSONValue {
    /// Convenience accessor for `data.message` on a control-plane event.
    var message: String? {
        if case .object(let object) = self, case .string(let message)? = object["message"] {
            return message
        }
        return nil
    }

    /// Convenience accessor for `data.state` on a control-plane event.
    var eventState: String? {
        if case .object(let object) = self, case .string(let state)? = object["state"] {
            return state
        }
        return nil
    }
}

// MARK: - Review

struct ControlPlaneRunReview: Decodable {
    let run: String?
    let resource: ControlPlaneRun
    let evidence: JSONValue?
}

// MARK: - Date formatting helpers

enum ControlPlaneDate {
    /// Homeboy core emits RFC 3339 timestamps with variable-precision
    /// fractional seconds (commonly 6 digits), which `ISO8601DateFormatter`
    /// does not reliably parse. Fall back to trimming the fraction when the
    /// fast path fails.
    static func parse(_ value: String?) -> Date? {
        guard let value, !value.isEmpty else { return nil }

        let withFractional = ISO8601DateFormatter()
        withFractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = withFractional.date(from: value) {
            return date
        }

        let withoutFractional = ISO8601DateFormatter()
        withoutFractional.formatOptions = [.withInternetDateTime]
        if let date = withoutFractional.date(from: value) {
            return date
        }

        // Trim an over-precise fractional component (more than 3 digits) and retry.
        if let dotIndex = value.firstIndex(of: "."),
           let offsetStart = value[dotIndex...].firstIndex(where: { $0 == "+" || $0 == "-" || $0 == "Z" }) {
            let fraction = value[value.index(after: dotIndex)..<offsetStart].prefix(3)
            let trimmed = value[..<dotIndex] + "." + fraction + value[offsetStart...]
            if let date = withFractional.date(from: String(trimmed)) {
                return date
            }
        }

        return nil
    }

    static func localTimeString(_ value: String?) -> String? {
        guard let date = parse(value) else { return nil }
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    /// Formats a timestamp relative to now, e.g. "in 3h" or "2d ago".
    static func relative(_ value: String?, referenceDate: Date = Date()) -> String? {
        guard let date = parse(value) else { return nil }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: date, relativeTo: referenceDate)
    }
}
