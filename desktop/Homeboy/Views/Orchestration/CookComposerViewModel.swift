import Foundation

/// Backs `CookComposerView`. Runs `homeboy agent-task cook` through
/// `CLIBridge` — once with `--preview` to validate non-mutating, once for
/// real to submit — and holds the field-level diagnostics, capacity
/// annotations, and submit-eligibility the composer renders.
///
/// Homeboy core remains the sole authority on validity, placement, and
/// provider selection: this view model only forwards the operator's form as
/// `homeboy agent-task cook` arguments and mirrors back what core reports.
@MainActor
final class CookComposerViewModel: ObservableObject {
    @Published var form = CookComposerForm()

    @Published private(set) var componentIDs: [String] = []
    @Published private(set) var modelOptions: [CookComposerModelOption] = []
    @Published private(set) var isLoadingContext = false

    @Published private(set) var isPreviewing = false
    @Published private(set) var isSubmitting = false

    @Published private(set) var previewData: CookPreviewData?
    /// The exact submit arguments (`buildArguments(includePreview: false)`)
    /// a successful preview last validated. Submit is only enabled while the
    /// form's current arguments still equal this snapshot; any edit changes
    /// `form.buildArguments(includePreview: false)` and invalidates it.
    @Published private(set) var lastPreviewedArguments: [String]?

    @Published var formLevelError: String?
    @Published private(set) var fieldErrors: [CookComposerField: String] = [:]
    @Published var error: (any DisplayableError)?

    /// Set once a real submission succeeds: the run to select in Missions.
    @Published private(set) var submittedRunID: String?

    private let cli: CLIBridge
    private let homeboyCLI: HomeboyCLI

    init(cli: CLIBridge = .shared, homeboyCLI: HomeboyCLI = .shared) {
        self.cli = cli
        self.homeboyCLI = homeboyCLI
    }

    // MARK: - Context (component IDs, model capacity)

    func loadContext() async {
        guard !isLoadingContext else { return }
        isLoadingContext = true
        defer { isLoadingContext = false }

        async let componentsResult: [ComponentListItemCLI]? = try? homeboyCLI.componentList()
        async let capacityResult: AgentTaskCapacityReport? = try? homeboyCLI.agentTaskCapacity()

        componentIDs = (await componentsResult)?.map(\.id).sorted() ?? []
        let routes = (await capacityResult)?.routes.map {
            CookComposerCapacityRoute(
                models: $0.models,
                headline: $0.capacity.headline,
                isExhausted: $0.capacity.state == "exhausted"
            )
        } ?? []
        modelOptions = CookComposerModelOption.options(from: routes)
    }

    /// A warning banner's text when the selected model's route is currently
    /// exhausted. This is only a warning: Homeboy's rotation decides the
    /// actual route at dispatch time, independent of what capacity reported
    /// when the composer loaded.
    var capacityWarning: String? {
        guard let modelID = form.modelID, !modelID.isEmpty else { return nil }
        guard let option = modelOptions.first(where: { $0.id == modelID }), option.isExhausted else {
            return nil
        }
        return "\(option.id) is \(option.headline). Homeboy's rotation may route elsewhere or wait."
    }

    // MARK: - Eligibility

    var canPreview: Bool { form.isValid && !isPreviewing && !isSubmitting }

    /// Submit is enabled only once a preview has succeeded for exactly the
    /// arguments the form currently builds. Any edit changes
    /// `buildArguments(includePreview: false)`, so this recomputes to
    /// `false` without any separate "dirty" flag to keep in sync.
    var canSubmit: Bool {
        guard !isSubmitting, !isPreviewing else { return false }
        return form.matchesPreviewedArguments(lastPreviewedArguments)
    }

    /// True once a successful preview exists but a later edit changed the
    /// arguments it validated. The composer shows this as "preview again"
    /// guidance rather than silently re-enabling Submit for stale evidence.
    var isPreviewStale: Bool {
        lastPreviewedArguments != nil && !form.matchesPreviewedArguments(lastPreviewedArguments)
    }

    // MARK: - Preview

    func preview() async {
        guard canPreview else { return }
        isPreviewing = true
        formLevelError = nil
        fieldErrors = [:]
        defer { isPreviewing = false }

        let args = ["agent-task", "cook"] + form.buildArguments(includePreview: true)
        do {
            let envelope = try await cli.executeJSONWithStdin(args, stdin: form.prompt, as: CookPreviewEnvelope.self)
            if envelope.success, let data = envelope.data {
                previewData = data
                lastPreviewedArguments = form.buildArguments(includePreview: false)
            } else {
                previewData = nil
                lastPreviewedArguments = nil
                applyDiagnostic(envelope.diagnostics, summary: envelope.summary, fallback: "Preview failed.")
            }
        } catch {
            previewData = nil
            lastPreviewedArguments = nil
            self.error = error.toDisplayableError(source: "New Cook")
        }
    }

    // MARK: - Submit

    func submit() async {
        guard canSubmit else { return }
        isSubmitting = true
        defer { isSubmitting = false }

        let args = ["agent-task", "cook"] + form.buildArguments(includePreview: false)
        do {
            let envelope = try await cli.executeJSONWithStdin(args, stdin: form.prompt, as: CookSubmitEnvelope.self)
            if envelope.success {
                submittedRunID = envelope.data?.missionSelectionID
            } else {
                applyDiagnostic(envelope.diagnostics, summary: envelope.summary, fallback: "Submit failed.")
            }
        } catch {
            self.error = error.toDisplayableError(source: "New Cook")
        }
    }

    /// Clears the field the operator is editing so a stale diagnostic does
    /// not linger next to a field the operator has already changed.
    func clearFieldError(_ field: CookComposerField) {
        fieldErrors[field] = nil
    }

    private func applyDiagnostic(_ diagnostics: CookDiagnostics?, summary: String?, fallback: String) {
        guard let diagnostics else {
            formLevelError = summary ?? fallback
            return
        }
        if let field = CookComposerField.matching(diagnosticMessage: diagnostics.message) {
            fieldErrors[field] = diagnostics.message
        } else {
            formLevelError = diagnostics.message
        }
    }
}
