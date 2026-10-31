import Foundation

/// Placement request for a Cook run: `auto` (the default) lets Homeboy's
/// placement policy choose; `local`/`lab` request a specific placement that
/// Homeboy still validates and may still refuse. Desktop never enforces
/// placement rules itself — this only shapes the `--placement` argument.
enum CookPlacementOption: String, CaseIterable, Identifiable, Hashable {
    case auto
    case local
    case lab

    var id: String { rawValue }

    var label: String {
        switch self {
        case .auto: return "Auto"
        case .local: return "Local"
        case .lab: return "Lab"
        }
    }
}

/// Every field the New Cook composer collects, one field per `homeboy
/// agent-task cook` flag. A pure value type with no CLI or SwiftUI
/// dependency, so argument building and submit-eligibility are directly
/// unit-testable (see `tests/CookComposerContractTests.swift`) without a
/// daemon, CLI binary, or view hierarchy.
struct CookComposerForm: Equatable {
    var repo: String = ""
    var taskURL: String = ""
    var goal: String = ""
    var prompt: String = ""
    var verifyGates: [String] = [""]
    var modelID: String?
    var placement: CookPlacementOption = .auto
    var draftPR: Bool = true

    /// Non-empty, trimmed verification gates. At least one is required; blank
    /// rows the operator has not filled in yet are never sent as `--verify`.
    var trimmedVerifyGates: [String] {
        verifyGates
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    /// Field-level reasons the form cannot be previewed yet, keyed the same
    /// way `CookComposerField` names fields so the UI can surface the message
    /// beside the field that blocks submission.
    var validationErrors: [CookComposerField: String] {
        var errors: [CookComposerField: String] = [:]
        if repo.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errors[.repo] = "Repository is required."
        }
        if taskURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errors[.taskURL] = "Tracker URL is required."
        }
        if goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errors[.goal] = "Goal is required."
        }
        if prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errors[.prompt] = "Prompt is required."
        }
        if trimmedVerifyGates.isEmpty {
            errors[.verify] = "At least one verification gate is required."
        }
        return errors
    }

    var isValid: Bool { validationErrors.isEmpty }

    /// Builds `homeboy agent-task cook` arguments, without the leading
    /// `agent-task cook` subcommand (the caller prepends that). Order is
    /// stable so two forms with identical field values always produce
    /// identical argument arrays, which is what submit-eligibility compares
    /// against the arguments a successful preview last validated.
    ///
    /// `--prompt -` is always emitted; the prompt text itself is never an
    /// argument. It is piped over stdin (`CLIBridge.executeWithStdin`) so
    /// shell quoting never applies to multiline or special-character prompts.
    func buildArguments(includePreview: Bool) -> [String] {
        var args: [String] = []
        args += ["--repo", repo.trimmingCharacters(in: .whitespacesAndNewlines)]
        args += ["--task-url", taskURL.trimmingCharacters(in: .whitespacesAndNewlines)]
        args += ["--goal", goal.trimmingCharacters(in: .whitespacesAndNewlines)]
        args += ["--prompt", "-"]
        for gate in trimmedVerifyGates {
            args += ["--verify", gate]
        }
        if let modelID, !modelID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            args += ["--model", modelID, "--acknowledge-model-override"]
        }
        if placement != .auto {
            args += ["--placement", placement.rawValue]
        }
        if draftPR {
            args.append("--draft-pr")
        }
        if includePreview {
            args.append("--preview")
        }
        return args
    }

    /// Whether this form's submit arguments (`buildArguments(includePreview:
    /// false)`) still equal the arguments a previous successful preview
    /// validated. `nil` means no preview has succeeded yet. Any field edit
    /// changes `buildArguments`'s output, so this recomputes to `false`
    /// without a separate "dirty" flag to keep in sync — the single source
    /// of truth for Submit's eligibility (`CookComposerViewModel.canSubmit`).
    func matchesPreviewedArguments(_ previousArguments: [String]?) -> Bool {
        guard let previousArguments else { return false }
        return previousArguments == buildArguments(includePreview: false)
    }
}

/// One form field the composer collects, named to match the diagnostic
/// vocabulary Homeboy core's validation errors use (`Invalid argument
/// 'model': ...`) so a failed preview's message can be attached to the field
/// that caused it instead of only shown as a generic form-level error.
enum CookComposerField: String, CaseIterable, Hashable {
    case repo
    case taskURL
    case goal
    case prompt
    case verify
    case model
    case workspace
    case placement
    case draftPR

    /// Core's own field-name vocabulary for the same concept, so a
    /// diagnostic naming `task_url`, `task-url`, `repository`, or `cwd`
    /// still resolves to the form field that collects it.
    private var diagnosticAliases: [String] {
        switch self {
        case .repo: return ["repo", "repository"]
        case .taskURL: return ["task_url", "task-url", "taskurl"]
        case .goal: return ["goal"]
        case .prompt: return ["prompt"]
        case .verify: return ["verify", "gate", "gates"]
        case .model: return ["model"]
        case .workspace: return ["workspace", "cwd", "to_worktree", "to-worktree"]
        case .placement: return ["placement"]
        case .draftPR: return ["draft_pr", "draft-pr"]
        }
    }

    /// Extracts the single-quoted flag name Homeboy core's
    /// `Invalid argument '<field>': ...` diagnostics carry, and maps it to
    /// the field that collects it. Returns `nil` when the message does not
    /// name a recognized field, so the caller shows a form-level error
    /// instead of misattaching it to an unrelated field.
    static func matching(diagnosticMessage message: String) -> CookComposerField? {
        guard let firstQuote = message.firstIndex(of: "'") else { return nil }
        let afterFirst = message.index(after: firstQuote)
        guard let secondQuote = message[afterFirst...].firstIndex(of: "'") else { return nil }
        let quoted = String(message[afterFirst..<secondQuote]).lowercased()

        return CookComposerField.allCases.first { field in
            field.diagnosticAliases.contains(quoted)
        }
    }
}

/// A capacity route's shape needed to build model options, decoupled from
/// `AgentTaskCapacityReport` (`Core/CLI/HomeboyCLI+MissionControlCommands.swift`)
/// so this file stays CLI-independent and directly testable. The view model
/// adapts the real capacity report into these before calling
/// `CookComposerModelOption.options(from:)`.
struct CookComposerCapacityRoute: Equatable {
    let models: [String]
    let headline: String
    let isExhausted: Bool
}

/// One selectable entry in the composer's Model picker, built from `homeboy
/// agent-task capacity`'s routes: every model behind a route is offered,
/// annotated with that route's live capacity headline (e.g. "60% remaining"
/// or "exhausted until <local time>"). Homeboy's rotation, not this list,
/// decides the route an actual cook uses; selecting a model here only adds
/// `--model` plus the required `--acknowledge-model-override`.
struct CookComposerModelOption: Identifiable, Equatable {
    let id: String
    let headline: String
    let isExhausted: Bool

    /// "anthropic/claude-sonnet-5 — 60% remaining".
    var displayName: String { "\(id) — \(headline)" }

    static func options(from routes: [CookComposerCapacityRoute]) -> [CookComposerModelOption] {
        var seen = Set<String>()
        var options: [CookComposerModelOption] = []
        for route in routes {
            for model in route.models {
                guard seen.insert(model).inserted else { continue }
                options.append(
                    CookComposerModelOption(id: model, headline: route.headline, isExhausted: route.isExhausted)
                )
            }
        }
        return options
    }
}

// MARK: - `agent-task cook --preview` / cook envelope decoding
//
// Mirrors the `homeboy/command-result/v3` envelope Homeboy core writes for
// `agent-task cook` (preview and real submission alike): top-level
// `success`/`summary`/`data`/`diagnostics`, decoded with
// `CLIBridgeResponse`'s `convertFromSnakeCase` decoder. Only the fields the
// composer renders or tests are declared; unrecognized JSON keys are ignored
// by `Decodable`, so new Homeboy fields never break decoding.

/// Preview's `data`, schema `homeboy/agent-task-cook-preview/v1`. A recorded
/// example is `tests/fixtures/agent-task-cook-preview.json`.
struct CookPreviewEnvelope: Decodable {
    let success: Bool
    let summary: String?
    let data: CookPreviewData?
    let diagnostics: CookDiagnostics?
}

struct CookPreviewData: Decodable {
    let schema: String?
    let mutates: Bool?
    let resolved: CookResolvedSummary?
    let replayArgv: [String]?
}

struct CookResolvedSummary: Decodable {
    let placement: CookResolvedPlacement?
    let provider: CookResolvedProvider?
    let gates: CookResolvedGates?
    let worktree: String?
    let workspace: CookResolvedWorkspace?
    let head: String?
    let base: String?
    let publication: CookResolvedPublication?
}

struct CookResolvedPlacement: Decodable {
    let requested: String?
    let selected: String?
}

struct CookResolvedProvider: Decodable {
    let backend: String?
    let model: String?
}

struct CookResolvedGates: Decodable {
    let publicCount: Int?
    let privateCount: Int?

    enum CodingKeys: String, CodingKey {
        case publicCount = "public"
        case privateCount = "private"
    }

    /// `public` + `private`: the gate count the summary line reports.
    var total: Int { (publicCount ?? 0) + (privateCount ?? 0) }
}

struct CookResolvedWorkspace: Decodable {
    let path: String?
    let branch: String?
    let handle: String?
}

struct CookResolvedPublication: Decodable {
    let draft: Bool?
    let finalize: Bool?
}

/// The `diagnostics` block Homeboy core's `CommandResultEnvelope` attaches on
/// `success: false`. Only `message` is required by core; `code` is retained
/// for display, matching the shape `CLIBridgeErrorDetail` already uses for
/// the generic CLI envelope.
struct CookDiagnostics: Decodable, Equatable {
    let code: String?
    let message: String
}

/// A real (non-preview) `agent-task cook` submission's envelope. Schema
/// `homeboy/agent-task-cook/v1` reports `cook_id`; `latest_run_id` is the
/// specific control-plane run to select in Missions when it differs from the
/// cook id.
struct CookSubmitEnvelope: Decodable {
    let success: Bool
    let summary: String?
    let data: CookSubmitData?
    let diagnostics: CookDiagnostics?
}

struct CookSubmitData: Decodable {
    let cookId: String?
    let latestRunId: String?

    /// The identifier to select in Missions: the specific run when Homeboy
    /// reported one, otherwise the cook id itself.
    var missionSelectionID: String? { latestRunId ?? cookId }
}
