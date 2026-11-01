use super::*;
use crate::agent_task::HOMEBOY_EMPTY_ATTEMPT_VERIFIED_KEY;
use crate::agent_task_cook_loop::AgentTaskIntentionalNoChange;

pub(super) fn normalize_provider_outcome_roles(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    strip_reserved_provider_metadata(outcome);
    surface_provider_run_result_diagnostics(outcome);
    normalize_provider_account_block(outcome, provider);
}

/// Normalize a provider result error through its adapter before assigning the
/// generic classification consumed by the scheduler.
fn normalize_provider_account_block(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    if outcome.failure_classification != Some(AgentTaskFailureClassification::ExecutionFailed) {
        return;
    }
    let Some(error) = outcome.metadata.get("provider_error") else {
        return;
    };
    let Some(error) = super::normalize_provider_error(&provider.backend, error) else {
        return;
    };
    let Some(classification) = super::normalized_error_failure_classification(&error) else {
        return;
    };

    outcome.failure_classification = Some(classification);
    push_unique_diagnostic(
        &mut outcome.diagnostics,
        "provider.capacity_rejected".to_string(),
        "Provider route rejected the request; retrying this route is disabled and the scheduler may rotate to the next configured provider."
            .to_string(),
        error,
    );
}

const HOMEBOY_ARTIFACT_PROVENANCE_KEY: &str = "artifact_provenance";
const HOMEBOY_INTENTIONAL_NO_CHANGE_VERIFIED_KEY: &str = "intentional_no_change_verified";

/// Provider payloads cannot assert Homeboy-owned verification or artifact
/// provenance. Those fields are added only after local validation.
fn strip_reserved_provider_metadata(outcome: &mut AgentTaskOutcome) {
    if let Some(metadata) = outcome.metadata.as_object_mut() {
        for key in [
            HOMEBOY_INTENTIONAL_NO_CHANGE_VERIFIED_KEY,
            HOMEBOY_EMPTY_ATTEMPT_VERIFIED_KEY,
        ] {
            metadata.remove(key);
        }
    }
    for artifact in &mut outcome.artifacts {
        if let Some(metadata) = artifact.metadata.as_object_mut() {
            metadata.remove(HOMEBOY_ARTIFACT_PROVENANCE_KEY);
        }
    }
    for typed in &mut outcome.typed_artifacts {
        if let Some(artifact) = &mut typed.artifact {
            if let Some(metadata) = artifact.metadata.as_object_mut() {
                metadata.remove(HOMEBOY_ARTIFACT_PROVENANCE_KEY);
            }
        }
    }
}

/// Records the actual size of regular files that Homeboy materialized for this
/// executor. Provider-supplied paths are otherwise treated as untrusted.
pub(super) fn normalize_homeboy_local_artifact_sizes(
    outcome: &mut AgentTaskOutcome,
    artifact_root: &std::path::Path,
    provenance: &AgentTaskArtifactsPathProvenance,
    workspace_root: Option<&std::path::Path>,
) {
    if provenance.owner != "homeboy" || provenance.locality != "runner" {
        return;
    }

    if let Some(metadata) = outcome.metadata.as_object_mut() {
        metadata.remove(HOMEBOY_INTENTIONAL_NO_CHANGE_VERIFIED_KEY);
    }

    let Ok(artifact_root) = artifact_root.canonicalize() else {
        return;
    };

    for artifact in &mut outcome.artifacts {
        measure_homeboy_local_artifact(artifact, &artifact_root);
    }
    for typed_artifact in &mut outcome.typed_artifacts {
        if let Some(artifact) = &mut typed_artifact.artifact {
            measure_homeboy_local_artifact(artifact, &artifact_root);
            if let Some(size_bytes) = artifact.size_bytes {
                if let Some(payload) = typed_artifact.payload.as_object_mut() {
                    payload.insert("size_bytes".to_string(), Value::from(size_bytes));
                }
            }
        }
    }

    if outcome.status == AgentTaskOutcomeStatus::Succeeded {
        classify_empty_change_outcome(outcome, workspace_root);
    }
}

/// Validate explicit review verdicts. Only harvest decides whether code changed.
pub(crate) fn classify_empty_change_outcome(
    outcome: &mut AgentTaskOutcome,
    workspace_root: Option<&std::path::Path>,
) {
    match intentional_no_change_verdict(outcome, workspace_root) {
        IntentionalNoChangeValidation::Verified => {
            // Preserve Succeeded. Cook applies task policy before deciding
            // whether this evidence-only result may terminalize.
            if !outcome.metadata.is_object() {
                outcome.metadata = json!({});
            }
            outcome.metadata[HOMEBOY_INTENTIONAL_NO_CHANGE_VERIFIED_KEY] = Value::Bool(true);
            outcome.summary = Some(
                "provider declared an intentional no-change review verdict bound to the verified workspace revision"
                    .to_string(),
            );
        }
        IntentionalNoChangeValidation::Invalid(message) => {
            outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
            outcome.summary = Some(message);
        }
        IntentionalNoChangeValidation::Absent => {}
    }
}

/// Verify a provider no-op against its checkout; artifact declarations are not proof.
pub(crate) fn accept_verified_unchanged_empty_attempt(
    outcome: &mut AgentTaskOutcome,
    workspace_root: Option<&std::path::Path>,
    attempt_base: Option<&str>,
) {
    let claims_no_change =
        outcome.status == AgentTaskOutcomeStatus::NoOp && outcome.outcome_candidate().is_none();
    if !claims_no_change || !verify_unchanged_empty_checkout(workspace_root, attempt_base) {
        return;
    }
    record_verified_unchanged_empty_attempt(outcome);
}

/// Record verified unchanged execution without manufacturing a patch artifact.
pub(super) fn materialize_verified_no_change_set(
    outcome: &mut AgentTaskOutcome,
    request: &AgentTaskRequest,
    _artifact_root: &std::path::Path,
    provenance: &AgentTaskArtifactsPathProvenance,
    workspace_root: Option<&std::path::Path>,
    attempt_base: Option<&str>,
) {
    let requires_patch = || {
        request
            .canonical_artifact_declarations()
            .iter()
            .any(|declaration| declaration.required && declaration.name == "patch")
    };
    // A provider that stopped without editing and without its review_form
    // claims no change exactly like a clean `succeeded` turn (#16002); the
    // scheduler's review_form deferral needs a candidate to defer.
    let claims_no_change = match outcome.status {
        AgentTaskOutcomeStatus::NoOp => true,
        AgentTaskOutcomeStatus::Succeeded => requires_patch(),
        AgentTaskOutcomeStatus::Failed => {
            requires_patch()
                && crate::agent_task_scheduler::AgentTaskScheduleSupport::review_form_only_deferral_gap(
                    outcome, request,
                )
                .is_some()
        }
        _ => false,
    };
    if !claims_no_change
        || provenance.owner != "homeboy"
        || provenance.locality != "runner"
        || outcome.outcome_candidate().is_some()
        || !verify_unchanged_empty_checkout(workspace_root, attempt_base)
    {
        return;
    }
    record_verified_unchanged_empty_attempt(outcome);
}

/// Whether an attempt checkout is provably unchanged from its attempt base:
/// `HEAD` is still the attempt base and the tree is clean apart from runner
/// metadata, so there is nothing to salvage and no candidate to gate (#15294).
pub(crate) fn verify_unchanged_empty_checkout(
    workspace_root: Option<&std::path::Path>,
    attempt_base: Option<&str>,
) -> bool {
    let (Some(workspace_root), Some(attempt_base)) = (workspace_root, attempt_base) else {
        return false;
    };
    let attempt_base = attempt_base.trim();
    if attempt_base.is_empty()
        || homeboy_core::git::output_allow_empty(workspace_root, &["rev-parse", "HEAD"]).as_deref()
            != Some(attempt_base)
    {
        return false;
    }
    let status_args = ["status", "--porcelain", "--untracked-files=all", "--", "."];
    // An unreadable status proves nothing; only a readable clean tree is
    // unchanged.
    homeboy_core::git::output_allow_empty(workspace_root, &status_args)
        .is_some_and(|status| status.is_empty())
}

/// Record a Homeboy-verified unchanged attempt exactly like
/// [`accept_verified_unchanged_empty_attempt`]: `Succeeded` with
/// [`HOMEBOY_EMPTY_ATTEMPT_VERIFIED_KEY`], so promotion reports a no-change
/// attempt without running gates and Cook's no-change feedback loop acts on it
/// (#15294/#15850).
pub(crate) fn record_verified_unchanged_empty_attempt(outcome: &mut AgentTaskOutcome) {
    outcome.status = AgentTaskOutcomeStatus::Succeeded;
    outcome.failure_classification = None;
    outcome.summary = Some(
        "provider produced no change; Homeboy verified the attempt checkout is unchanged from its base"
            .to_string(),
    );
    if !outcome.metadata.is_object() {
        outcome.metadata = json!({});
    }
    outcome.metadata[HOMEBOY_EMPTY_ATTEMPT_VERIFIED_KEY] = Value::Bool(true);
}

const INTENTIONAL_NO_CHANGE_SCHEMA: &str = "homeboy/intentional-no-change/v1";

enum IntentionalNoChangeValidation {
    Absent,
    Verified,
    Invalid(String),
}

/// A provider may declare a no-change review only through this versioned result
/// field. Homeboy, rather than the provider, verifies the declared revision and
/// the complete checkout state before allowing transcript evidence to be a
/// successful no-op.
fn intentional_no_change_verdict(
    outcome: &AgentTaskOutcome,
    workspace_root: Option<&std::path::Path>,
) -> IntentionalNoChangeValidation {
    let Some(verdict) = outcome
        .outputs
        .get("provider_run_result")
        .and_then(|result| result.get("intentional_no_change"))
    else {
        return IntentionalNoChangeValidation::Absent;
    };
    let Ok(declaration) = serde_json::from_value::<AgentTaskIntentionalNoChange>(verdict.clone())
    else {
        return IntentionalNoChangeValidation::Invalid(
            "provider declared an intentional no-change verdict without a valid schema, supported disposition, and inspected workspace revision; changes require recovery review"
                .to_string(),
        );
    };
    if declaration.schema != INTENTIONAL_NO_CHANGE_SCHEMA
        || declaration.inspected_revision.trim().is_empty()
    {
        return IntentionalNoChangeValidation::Invalid(
            "provider declared an invalid intentional no-change verdict; changes require recovery review"
                .to_string(),
        );
    }
    let Some(workspace_root) = workspace_root else {
        return IntentionalNoChangeValidation::Invalid(
            "provider declared an intentional no-change verdict but Homeboy has no workspace checkout to verify; changes require recovery review"
                .to_string(),
        );
    };
    let head = homeboy_core::git::output_allow_empty(workspace_root, &["rev-parse", "HEAD"]);
    if head.as_deref() != Some(declaration.inspected_revision.as_str()) {
        return IntentionalNoChangeValidation::Invalid(
            "provider declared an intentional no-change verdict for a revision that does not match the workspace checkout; changes require recovery review"
                .to_string(),
        );
    }
    if homeboy_core::git::output_allow_empty(
        workspace_root,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .is_none_or(|status| !status.is_empty())
    {
        return IntentionalNoChangeValidation::Invalid(
            "provider declared an intentional no-change verdict but the workspace checkout changed; changes require recovery review"
                .to_string(),
        );
    }
    IntentionalNoChangeValidation::Verified
}

fn measure_homeboy_local_artifact(
    artifact: &mut AgentTaskArtifact,
    artifact_root: &std::path::Path,
) {
    let Some(path) = artifact.path.as_deref() else {
        return;
    };
    let path = std::path::Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        artifact_root.join(path)
    };
    let Ok(path) = path.canonicalize() else {
        return;
    };
    if !path.starts_with(artifact_root) {
        return;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_file() {
        artifact.size_bytes = Some(metadata.len());
    }
}

pub(crate) fn has_known_empty_change_set(outcome: &AgentTaskOutcome) -> bool {
    outcome.outcome_candidate().is_none()
}

/// Whether an outcome status represents a failure for which we want to mine the
/// provider run-result for actionable evidence.
fn is_failure_status(status: AgentTaskOutcomeStatus) -> bool {
    matches!(
        status,
        AgentTaskOutcomeStatus::Failed
            | AgentTaskOutcomeStatus::ProviderError
            | AgentTaskOutcomeStatus::Timeout
            | AgentTaskOutcomeStatus::UnableToRemediate
    )
}

/// Surface actionable provider run-result evidence into the outcome on failure
/// (#4105).
///
/// Provider executors emit a
/// structured run-result under `outputs.provider_run_result` following the
/// `*/agent-task-run-result/*` shape: `{status, failure_classification,
/// diagnostics[], artifacts[], metadata{provider_error,run_id,run_status,
/// runtime_id,runtime_status}, refs{logs,transcripts,artifact_bundles,...}}`.
///
/// Before this fix a FAILED run-result could be preserved verbatim while
/// homeboy surfaced nothing actionable — `agent-task logs/artifacts/review`
/// showed only the generic "agent task failed" summary even though the
/// run-result carried (or conspicuously lacked) provider error codes, a run /
/// runtime id, and log / transcript refs.
///
/// This walks the preserved run-result and ADDS (never overwrites) the
/// following to the outcome so operators get actionable info:
/// - each run-result `diagnostics[]` entry becomes an outcome diagnostic;
/// - `metadata.provider_error` + run/runtime ids + statuses become a single
///   `provider.run_result_failed` diagnostic and are mirrored onto
///   `outcome.metadata.provider_error`;
/// - `refs.{logs,transcripts,artifact_bundles,runtimes,patches}` become
///   `evidence_refs` so review/artifacts can surface them;
/// - if the run-result is an empty shell (no diagnostics, no provider_error, no
///   run/runtime id, no refs) a single reviewer-safe diagnostic explains that
///   no provider runtime/session was created, satisfying the acceptance rule
///   that a failed run-result is never an empty shell.
///
/// It is fully provider-agnostic: it keys only off the generic run-result shape
/// and never references any specific runtime, framework, or provider id.
pub(super) fn surface_provider_run_result_diagnostics(outcome: &mut AgentTaskOutcome) {
    if !is_failure_status(outcome.status) {
        return;
    }
    let Some(run_result) = output_value(&outcome.outputs, "provider_run_result").cloned() else {
        return;
    };
    let Some(run_result) = run_result.as_object() else {
        return;
    };

    // Only mine FAILED (or non-succeeded) run-results. A run-result that omits
    // status is treated as a failure here because the outcome itself already
    // failed.
    let run_status = run_result
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    if matches!(run_status.as_deref(), Some("succeeded") | Some("success")) {
        return;
    }

    let mut surfaced_evidence = false;

    // 1. Lift each run-result diagnostic into the outcome diagnostics, deduped
    //    by (class, message) against what is already present.
    if let Some(Value::Array(items)) = run_result.get("diagnostics") {
        for item in items {
            let Some(message) = item
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if message.trim().is_empty() {
                continue;
            }
            let class = item
                .get("class")
                .or_else(|| item.get("kind"))
                .or_else(|| item.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("provider.run_result_diagnostic")
                .to_string();
            push_unique_diagnostic(
                &mut outcome.diagnostics,
                class,
                message,
                item.get("data").cloned().unwrap_or(Value::Null),
            );
            surfaced_evidence = true;
        }
    }

    // 2. Pull the structured failure metadata (provider error + run/runtime
    //    identity + statuses) into a single actionable diagnostic and mirror
    //    provider_error onto the outcome metadata.
    let metadata = run_result.get("metadata").and_then(Value::as_object);
    let provider_error = metadata
        .and_then(|map| map.get("provider_error"))
        .filter(|value| !is_empty_value(value))
        .cloned();
    let run_id = metadata
        .and_then(|map| map.get("run_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_id = metadata
        .and_then(|map| map.get("runtime_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let runtime_status = metadata
        .and_then(|map| map.get("runtime_status"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);

    let has_identity = provider_error.is_some()
        || run_id.is_some()
        || runtime_id.is_some()
        || runtime_status.is_some();

    if has_identity {
        let message = describe_run_result_failure(
            run_status.as_deref(),
            run_id.as_deref(),
            runtime_id.as_deref(),
            runtime_status.as_deref(),
            provider_error.as_ref(),
        );
        let data = json!({
            "run_status": run_status,
            "run_id": run_id,
            "runtime_id": runtime_id,
            "runtime_status": runtime_status,
            "provider_error": provider_error,
        });
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_failed".to_string(),
            message,
            data,
        );
        surfaced_evidence = true;

        if let Some(provider_error) = provider_error {
            mirror_provider_error_metadata(outcome, provider_error);
        }
    }

    // 3. Promote run-result refs (logs, transcripts, artifact bundles, runtimes,
    //    patches) into evidence refs so review/artifacts can surface them.
    if let Some(refs) = run_result.get("refs").and_then(Value::as_object) {
        for (group, kind) in [
            ("logs", "provider-log"),
            ("transcripts", "provider-transcript"),
            ("artifact_bundles", "provider-artifact-bundle"),
            ("runtimes", "provider-runtime"),
            ("patches", "provider-patch"),
        ] {
            let Some(Value::Array(entries)) = refs.get(group) else {
                continue;
            };
            for entry in entries {
                if let Some(reference) = run_result_ref_uri(entry) {
                    push_unique_evidence_ref(&mut outcome.evidence_refs, kind, reference, group);
                    surfaced_evidence = true;
                }
            }
        }
    }

    // 4. Empty shell guard: a failed run-result that surfaced no diagnostics,
    //    no provider error, no run/runtime identity, and no refs must still
    //    explain itself rather than appear as an opaque empty failure.
    if !surfaced_evidence {
        push_unique_diagnostic(
            &mut outcome.diagnostics,
            "provider.run_result_empty".to_string(),
            "Provider run-result reported failure but produced no diagnostics, \
             provider error, run/runtime id, or log/transcript refs: no provider \
             runtime or session appears to have been created."
                .to_string(),
            json!({ "run_status": run_status }),
        );
    }
}

/// Treat `null`, empty string, empty object, and empty array as "no value" so an
/// empty `provider_error: {}` shell is not mistaken for real evidence.
fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Build a human-readable failure message from the run-result identity fields.
fn describe_run_result_failure(
    run_status: Option<&str>,
    run_id: Option<&str>,
    runtime_id: Option<&str>,
    runtime_status: Option<&str>,
    provider_error: Option<&Value>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(error) = provider_error {
        if let Some(message) = provider_error_message(error) {
            parts.push(format!("provider error: {message}"));
        } else {
            parts.push("provider error reported".to_string());
        }
    }
    if let Some(run_id) = run_id {
        parts.push(format!("run_id={run_id}"));
    }
    if let Some(runtime_id) = runtime_id {
        parts.push(format!("runtime_id={runtime_id}"));
    }
    if let Some(runtime_status) = runtime_status {
        parts.push(format!("runtime_status={runtime_status}"));
    }
    if parts.is_empty() {
        return format!(
            "Provider run-result failed (status={}).",
            run_status.unwrap_or("failed")
        );
    }
    format!("Provider run-result failed: {}.", parts.join(", "))
}

/// Extract a short error message from a provider_error value, accepting either a
/// string or an object carrying `message`/`error`/`detail`/`code`.
fn provider_error_message(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("message")
            .or_else(|| map.get("error"))
            .or_else(|| map.get("detail"))
            .or_else(|| map.get("code"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Mirror the provider_error onto `outcome.metadata.provider_error` without
/// clobbering an existing populated value.
fn mirror_provider_error_metadata(outcome: &mut AgentTaskOutcome, provider_error: Value) {
    let mut metadata = outcome.metadata.as_object().cloned().unwrap_or_default();
    let already_populated = metadata
        .get("provider_error")
        .is_some_and(|existing| !is_empty_value(existing));
    if !already_populated {
        metadata.insert("provider_error".to_string(), provider_error);
        outcome.metadata = Value::Object(metadata);
    }
}

/// Resolve a usable reference URI/path from a run-result ref entry, accepting a
/// bare string or an object carrying `uri`/`url`/`path`/`ref`/`id`.
fn run_result_ref_uri(entry: &Value) -> Option<String> {
    match entry {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Object(map) => map
            .get("uri")
            .or_else(|| map.get("url"))
            .or_else(|| map.get("path"))
            .or_else(|| map.get("ref"))
            .or_else(|| map.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Push a diagnostic unless an identical (class, message) is already present.
pub(super) fn push_unique_diagnostic(
    diagnostics: &mut Vec<AgentTaskDiagnostic>,
    class: String,
    message: String,
    data: Value,
) {
    if diagnostics
        .iter()
        .any(|existing| existing.class == class && existing.message == message)
    {
        return;
    }
    diagnostics.push(AgentTaskDiagnostic {
        class,
        message,
        data,
    });
}

/// Push an evidence ref unless an identical (kind, uri) is already present.
fn push_unique_evidence_ref(
    evidence_refs: &mut Vec<AgentTaskEvidenceRef>,
    kind: &str,
    uri: String,
    group: &str,
) {
    if evidence_refs
        .iter()
        .any(|existing| existing.kind == kind && existing.uri == uri)
    {
        return;
    }
    evidence_refs.push(AgentTaskEvidenceRef {
        kind: kind.to_string(),
        uri,
        label: Some(format!("provider run-result {group}")),
    });
}

fn output_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| !value.is_null())
}
