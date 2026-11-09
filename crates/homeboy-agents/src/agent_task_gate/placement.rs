//! Gate placement belongs to the controller, before selecting private HOME.
//! The runner transport is supplied by the existing Lab execution owner.

use super::*;
use crate::agent_task_promotion::{candidate_fingerprint, AgentTaskPromotionCandidate};

pub const LAB_GATE_RECEIPT_SCHEMA: &str = "homeboy/admitted-lab-gate-receipt/v1";
pub const LAB_GATE_DISPOSITION_SCHEMA: &str = "homeboy/lab-gate-disposition/v1";

/// Seal only the declared extension/shared closure using the gate owner's
/// existing copy, path, overlap and identity checks. No registry is exported.
pub fn seal_extension_resources(
    root: &Path,
    inputs: &[AgentTaskGateExtensionInput],
) -> Result<(
    Vec<AgentTaskGateExtensionInput>,
    Vec<AgentTaskGateExtensionInputProvenance>,
)> {
    let home = root.join("validated-home");
    fs::create_dir_all(&home).map_err(|error| Error::internal_io(error.to_string(), None))?;
    let mut report = AgentTaskGateEnvironment::default();
    materialize_extension_inputs(&mut report, &home, inputs)?;
    let mut sealed = Vec::new();
    for input in &report.extension_inputs {
        let package = root.join("packages").join(&input.id);
        let source = package.join(&input.id);
        copy_extension_input(&home.join(&input.destination), &source)?;
        for asset in &input.shared_assets {
            let destination = homeboy_core::resolve_contained_local_path(
                &package,
                &asset.path,
                "gate.shared_asset",
            )?;
            copy_extension_input(&home.join(&asset.destination), &destination)?;
        }
        fs::write(package.join("homeboy-extension-root.json"), serde_json::json!({"shared_assets": input.shared_assets.iter().map(|asset| &asset.path).collect::<Vec<_>>()}).to_string())
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        sealed.push(AgentTaskGateExtensionInput {
            id: input.id.clone(),
            source: source.display().to_string(),
            identity: Some(input.identity.clone()),
        });
    }
    fs::remove_dir_all(home).map_err(|error| Error::internal_io(error.to_string(), None))?;
    Ok((sealed, report.extension_inputs))
}

/// Compare canonical materialization evidence while allowing source-path remaps.
/// Extension/shared cardinalities, identities and private destinations all belong
/// to the existing gate resource owner; no separate closure hash is introduced.
pub fn verify_extension_resource_closure(
    expected: &[AgentTaskGateExtensionInputProvenance],
    actual: &[AgentTaskGateExtensionInputProvenance],
) -> Result<()> {
    if expected.len() != actual.len()
        || expected.iter().zip(actual).any(|(expected, actual)| {
            expected.id != actual.id
                || expected.identity != actual.identity
                || expected.destination != actual.destination
                || expected.shared_assets.len() != actual.shared_assets.len()
                || expected
                    .shared_assets
                    .iter()
                    .zip(&actual.shared_assets)
                    .any(|(expected, actual)| {
                        expected.path != actual.path
                            || expected.identity != actual.identity
                            || expected.destination != actual.destination
                    })
        })
    {
        return Err(Error::invalid_argument(
            "gate.resources",
            "materialized extension/shared closure differs from the sealed original",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LabGateRequestSchema {
    #[serde(rename = "homeboy/agent-task-lab-gate-request/v2")]
    V2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabGateRequest {
    pub schema: LabGateRequestSchema,
    #[serde(default)]
    pub preflight_only: bool,
    #[serde(default)]
    pub toolchains: Vec<AgentTaskGateToolchainRequirement>,
    pub runner_id: String,
    pub candidate: AgentTaskPromotionCandidate,
    pub index: usize,
    pub argv: Vec<String>,
    pub label: String,
    pub visibility: AgentTaskGateVisibility,
    pub reveal_policy: AgentTaskGateRevealPolicy,
    pub environment: AgentTaskGateEnvironmentPolicy,
    pub package_artifacts: Vec<AgentTaskGatePackageArtifactRequirement>,
    pub declared_plan: Option<homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    pub timeout_seconds: u64,
    pub no_progress_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabGateReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    pub schema: String,
    pub request_sha256: String,
    pub candidate: AgentTaskPromotionCandidate,
    pub execution_context: serde_json::Value,
    pub report: AgentTaskGateReport,
    /// Dependency setup the runner performed in the materialized gate
    /// workspace before the gate command ran (#16262).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<AgentTaskGateSetupEvidence>,
}

/// A materialized gate workspace that is not the admitted candidate. Git
/// workspaces also carry the typed tree difference.
fn bounded_mismatch_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .take(50)
        .map(|path| homeboy_core::transport_json::bounded_redacted(path, 256))
        .collect()
}

#[cfg(test)]
#[test]
fn agent_task_gate_bounded_mismatch_paths() {
    for count in [0, 1, 50, 75] {
        let paths = vec![format!("https://user:password@example.com/{}", "界".repeat(300)); count];
        let bounded = bounded_mismatch_paths(&paths);
        assert_eq!(bounded.len(), count.min(50));
        for path in bounded {
            assert!(path.chars().count() <= 256);
            assert!(!path.contains("password"));
            assert!(!path.contains('�'));
        }
    }
}

struct CandidateMismatch {
    error: Error,
    mutation: Option<AgentTaskGateWorkspaceMutation>,
}

impl LabGateRequest {
    pub fn identity(&self) -> Result<String> {
        serde_json::to_vec(self)
            .map(|bytes| {
                format!(
                    "sha256:{}",
                    homeboy_engine_primitives::content_hash::sha256_hex(&bytes)
                )
            })
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("encode Lab gate request".to_string()),
                )
            })
    }

    /// Snapshot materialization may change the index projection and absolute
    /// path, but must retain the recorded HEAD and exact candidate tree.
    pub fn verify_materialized_candidate(&self, cwd: &Path) -> Result<()> {
        match self.materialized_candidate_mismatch(cwd)? {
            Some(mismatch) => Err(mismatch.error),
            None => Ok(()),
        }
    }

    fn materialized_candidate_mismatch(&self, cwd: &Path) -> Result<Option<CandidateMismatch>> {
        let actual = candidate_fingerprint(&cwd.display().to_string())?;
        let (expected, actual) = match (&self.candidate, &actual) {
            (
                AgentTaskPromotionCandidate::Git {
                    fingerprint: expected,
                },
                AgentTaskPromotionCandidate::Git {
                    fingerprint: actual,
                },
            ) => {
                if expected.head == actual.head
                    && !expected.tree.is_empty()
                    && expected.tree == actual.tree
                {
                    return Ok(None);
                }
                (expected, actual)
            }
            _ => {
                return Ok(Some(CandidateMismatch {
                    error: Error::invalid_argument(
                        "gate.candidate",
                        "Lab gate workspace differs from the controller candidate",
                    ),
                    mutation: None,
                }))
            }
        };
        let mut mutation = AgentTaskGateWorkspaceMutation {
            differing_paths: Vec::new(),
            expected_tree: expected.tree.clone(),
            actual_tree: actual.tree.clone(),
            path_comparison_error: None,
        };
        // Canonical candidate commits travel in the Git closure. Do not
        // substitute changed_files: those are relative to HEAD, not to the
        // admitted tree, and can hide missing tracked content.
        match homeboy_core::git::run_git_bytes_with_env(
            cwd,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                &expected.tree,
                &actual.tree,
                "--",
            ],
            "compare admitted and materialized gate trees",
            &[],
        ) {
            Ok(bytes) => {
                mutation.differing_paths = bytes
                    .split(|byte| *byte == 0)
                    .filter(|path| !path.is_empty())
                    .map(|path| String::from_utf8_lossy(path).into_owned())
                    .collect();
            }
            Err(cause) => mutation.path_comparison_error = Some(cause.message),
        }
        let mut error = Error::invalid_argument(
            "gate.candidate",
            "Lab gate workspace differs from the controller candidate",
        );
        let diagnostic_paths = bounded_mismatch_paths(&mutation.differing_paths);
        if !diagnostic_paths.is_empty() {
            error
                .message
                .push_str(&format!(": {}", diagnostic_paths.join(", ")));
        }
        error.details["expected_head"] = serde_json::json!(expected.head);
        error.details["actual_head"] = serde_json::json!(actual.head);
        error.details["expected_tree"] = serde_json::json!(mutation.expected_tree);
        error.details["actual_tree"] = serde_json::json!(mutation.actual_tree);
        match &mutation.path_comparison_error {
            Some(cause) => error.details["path_comparison_error"] = serde_json::json!(cause),
            None => error.details["differing_paths"] = serde_json::json!(diagnostic_paths),
        }
        Ok(Some(CandidateMismatch {
            error,
            mutation: Some(mutation),
        }))
    }

    pub fn verify_receipt(&self, receipt: &LabGateReceipt) -> Result<()> {
        if receipt.schema != LAB_GATE_RECEIPT_SCHEMA
            || receipt.request_sha256 != self.identity()?
            || receipt.candidate != self.candidate
            || receipt.report.id != format!("gate-{}", self.index)
        {
            return Err(Error::invalid_argument(
                "gate.receipt",
                "Lab gate receipt is not bound to the admitted candidate and invocation",
            ));
        }
        let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::from_evidence_record(&receipt.execution_context)?;
        if context.runner_id() != self.runner_id
            || receipt
                .execution_context
                .pointer("/context/verification/state")
                .and_then(serde_json::Value::as_str)
                != Some("verified")
        {
            return Err(Error::invalid_argument(
                "gate.receipt",
                "Lab gate receipt has the wrong execution owner",
            ));
        }
        Ok(())
    }
}

/// Freshness of the runner daemon observation behind one gate admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabGateFreshness {
    Fresh,
    /// The daemon lease or freshness report has not been observed yet.
    NotObserved,
    /// A daemon fault refuses admission (for example `version_drift`).
    Refused(String),
}

impl LabGateFreshness {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Fresh => "fresh",
            Self::NotObserved => "not_observed",
            Self::Refused(code) => code,
        }
    }
}

/// Reasons waiting can clear: capacity, pressure and a freshness that is not
/// yet observed. Any other `RunnerAvailability` reason defers at once.
const TRANSIENT_ADMISSION_REASONS: &[&str] = &[
    "capacity_reached",
    "capacity_unknown",
    "pressure",
    "daemon_freshness_unavailable",
    "daemon_unverified",
    "active_jobs_unavailable",
    "active_jobs_not_queried",
];

/// One observation of a runner's Lab admission, carrying the same
/// `RunnerAvailability` reasons `runner status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabGateAdmission {
    pub accepts_jobs: bool,
    pub reasons: Vec<String>,
    pub freshness: LabGateFreshness,
    pub next_action: Option<String>,
}

impl LabGateAdmission {
    pub fn accepting() -> Self {
        Self {
            accepts_jobs: true,
            reasons: Vec::new(),
            freshness: LabGateFreshness::Fresh,
            next_action: None,
        }
    }

    /// Whether re-observing can admit the gate without anyone acting on the
    /// runner. Disconnected, stale and drifted daemons never are.
    pub fn is_transient(&self) -> bool {
        !self.accepts_jobs
            && !self.reasons.is_empty()
            && !matches!(self.freshness, LabGateFreshness::Refused(_))
            && self
                .reasons
                .iter()
                .all(|reason| TRANSIENT_ADMISSION_REASONS.contains(&reason.as_str()))
    }

    /// The typed admission refusal, naming the reasons and freshness state.
    pub fn refusal(&self, runner_id: &str, waited: Option<Duration>) -> Error {
        let reasons = if self.reasons.is_empty() {
            "none reported".to_string()
        } else {
            self.reasons.join(", ")
        };
        let mut message = format!(
            "Lab gate runner `{runner_id}` cannot accept jobs (reasons: {reasons}; daemon freshness: {})",
            self.freshness.as_str()
        );
        if let Some(waited) = waited {
            message.push_str(&format!(
                " after waiting {}s for admission",
                waited.as_secs()
            ));
        }
        if let Some(next_action) = &self.next_action {
            message.push_str(&format!("; next: {next_action}"));
        }
        Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            message,
            serde_json::json!({
                "gate_disposition": "unavailable",
                "stage": "admission",
                "runner_id": runner_id,
                "reasons": self.reasons,
                "freshness": self.freshness.as_str(),
                "transient": self.is_transient(),
                "waited_seconds": waited.map(|waited| waited.as_secs()),
                "next_action": self.next_action,
            }),
        )
    }
}

const LAB_GATE_ADMISSION_INITIAL_BACKOFF: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(2)
};
const LAB_GATE_ADMISSION_MAX_BACKOFF: Duration = Duration::from_secs(30);

fn lab_gate_cancelled_before_admission() -> Error {
    let mut error =
        Error::invalid_argument("gate.placement", "Lab gate was cancelled before admission");
    error.details["gate_disposition"] = serde_json::json!("cancelled");
    error.details["stage"] = serde_json::json!("admission");
    error
}

/// Wait for the runner to admit the gate while its refusal is transient,
/// re-observing with bounded backoff inside the gate's timeout and
/// no-progress budget. Supervision cancellation stops the wait.
fn await_lab_gate_admission(
    request: &LabGateRequest,
    supervision: Option<&GateSupervision>,
) -> Result<()> {
    let transport = active_transport();
    let mut budget = Duration::from_secs(
        request
            .timeout_seconds
            .min(request.no_progress_timeout_seconds)
            .max(1),
    );
    if let Some(policy) = supervision {
        budget = budget.min(policy.timeout).min(policy.no_progress_timeout);
    }
    let cancelled = || supervision.is_some_and(|policy| (policy.is_cancelled)());
    let started = Instant::now();
    let mut backoff = LAB_GATE_ADMISSION_INITIAL_BACKOFF;
    loop {
        if cancelled() {
            return Err(lab_gate_cancelled_before_admission());
        }
        let admission = transport
            .admission(&request.runner_id)
            .map_err(|mut error| {
                if !error.details.is_object() {
                    error.details = serde_json::json!({});
                }
                error.details["stage"] = serde_json::json!("admission");
                error
            })?;
        if admission.accepts_jobs {
            return Ok(());
        }
        let remaining = budget.saturating_sub(started.elapsed());
        if !admission.is_transient() || remaining.is_zero() {
            let waited = (!started.elapsed().is_zero() && admission.is_transient())
                .then(|| started.elapsed());
            return Err(admission.refusal(&request.runner_id, waited));
        }
        if let Some(policy) = supervision {
            (policy.on_heartbeat)(&AgentTaskGateLiveStatus {
                visibility: request.visibility,
                reveal_policy: request.reveal_policy,
                elapsed_ms: started.elapsed().as_millis(),
                last_progress_ms_ago: None,
                progress: None,
                output_tail: format!(
                    "waiting for Lab runner admission (reasons: {})",
                    admission.reasons.join(", ")
                ),
            })?;
        }
        let wake = Instant::now() + backoff.min(remaining);
        while Instant::now() < wake && !cancelled() {
            std::thread::sleep(
                wake.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(250)),
            );
        }
        backoff = backoff
            .saturating_mul(2)
            .min(LAB_GATE_ADMISSION_MAX_BACKOFF);
    }
}

pub trait LabGateTransport: Send + Sync {
    fn declared_runner(&self, _command: &str) -> Result<Option<String>> {
        Ok(None)
    }
    /// Observe the runner's Lab admission before the gate is handed off.
    /// Transports without a runner admission owner admit at once.
    fn admission(&self, _runner_id: &str) -> Result<LabGateAdmission> {
        Ok(LabGateAdmission::accepting())
    }
    fn execute(
        &self,
        cwd: &Path,
        request: &LabGateRequest,
        supervision: Option<&GateSupervision>,
    ) -> Result<LabGateReceipt>;
}

pub(crate) fn declared_runner(command: &str) -> Result<Option<String>> {
    active_transport().declared_runner(command)
}

/// Enters the Lab runner's identity scope; the scope ends when the guard drops.
pub type RunnerIdentityScope = fn() -> Box<dyn std::any::Any>;
static RUNNER_IDENTITY_SCOPE: std::sync::RwLock<Option<RunnerIdentityScope>> =
    std::sync::RwLock::new(None);

pub fn register_runner_identity_scope(enter: RunnerIdentityScope) {
    *RUNNER_IDENTITY_SCOPE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(enter);
}

/// Hold for one gate round, admission pass or promotion attempt: each runner's
/// Homeboy identity is then probed once. Refresh still suspends the scope.
pub(crate) fn runner_identity_scope() -> Option<Box<dyn std::any::Any>> {
    let enter = *RUNNER_IDENTITY_SCOPE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    enter.map(|enter| enter())
}

struct Unavailable;
impl LabGateTransport for Unavailable {
    fn execute(
        &self,
        _: &Path,
        _: &LabGateRequest,
        _: Option<&GateSupervision>,
    ) -> Result<LabGateReceipt> {
        Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "admitted Lab gate execution transport is unavailable",
            serde_json::json!({"gate_disposition":"unavailable", "stage":"transport_registration"}),
        ))
    }
}

homeboy_engine_primitives::provider_registry_arc! {
    provider: dyn LabGateTransport,
    noop: Unavailable,
    register: pub fn register_lab_gate_transport,
    active: pub(crate) fn active_transport,
}

pub(super) fn dispatch(
    cwd: &Path,
    request: &LabGateRequest,
    supervision: Option<&GateSupervision>,
) -> Result<AgentTaskGateReport> {
    if let Err(error) = await_lab_gate_admission(request, supervision) {
        let error = gate_execute_unavailable(error, "admission", None);
        return Ok(non_execution_report(request, &error));
    }
    let mut setup_attempts = Vec::new();
    let mut receipt = None;
    for attempt in 0..2 {
        let result = match active_transport().execute(cwd, request, supervision) {
            Ok(receipt) => receipt,
            Err(error) => {
                let error = gate_execute_unavailable(error, "transport", None);
                return Ok(non_execution_report(request, &error));
            }
        };
        // A receipt the controller cannot bind is wrapper evidence, not a
        // verdict on the candidate (#17649).
        if let Err(error) = request.verify_receipt(&result) {
            let error = gate_execute_unavailable(
                error,
                "receipt_verification",
                result.artifact_ref.as_deref(),
            );
            return Ok(non_execution_report(request, &error));
        }
        let failed = result
            .setup
            .iter()
            .any(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed);
        let cancelled = result.setup.iter().any(|outcome| {
            outcome.termination == homeboy_core::deps::DependencyHydrationTermination::Cancelled
        });
        setup_attempts.extend(result.setup.iter().cloned());
        receipt = Some(result);
        if !failed
            || cancelled
            || supervision.is_some_and(|policy| (policy.is_cancelled)())
            || attempt == 1
        {
            break;
        }
    }
    let receipt = receipt.expect("setup always has an initial attempt");
    // A runner receipt cannot authorize a destination edited during transport.
    if candidate_fingerprint(&cwd.display().to_string())? != request.candidate {
        return Err(Error::invalid_argument(
            "gate.candidate",
            "controller candidate changed during Lab gate execution",
        ));
    }
    // The runner owns dependency setup for its gates: a failed setup never ran
    // the gate, so it is retryable gate setup, not candidate code (#16262).
    if let Some(failed) = receipt
        .setup
        .iter()
        .find(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed)
    {
        let mut error =
            crate::agent_task_promotion::gate_setup_outcome_failure("lab_gate_setup", failed);
        // The runner names the spent Lab gate limit and the elapsed setup
        // time in the timed-out outcome's reason (#16938).
        if failed.termination == homeboy_core::deps::DependencyHydrationTermination::TimedOut {
            error.message = format!("{}; {}", error.message, failed.reason);
        }
        error.details["artifact_ref"] = serde_json::json!(receipt.artifact_ref);
        let mut report = non_execution_report(request, &error);
        report.setup = setup_attempts;
        return Ok(report);
    }
    // `failed` means the declared command ran and returned non-zero (or
    // rewrote the candidate it ran on). Any other failed receipt is the
    // wrapper's own failure, so the gate is unavailable (#17649).
    if receipt.report.status == AgentTaskGateStatus::Failed
        && receipt.report.exit_code == 0
        && receipt.report.workspace_mutation.is_none()
    {
        let mut details = serde_json::json!({});
        if receipt.report.visibility != AgentTaskGateVisibility::Private {
            details["stderr_tail"] = serde_json::json!(text_tail(&receipt.report.stderr, 20));
        }
        let error = gate_execute_unavailable(
            Error::new(
                homeboy_core::ErrorCode::InternalUnexpected,
                "gate-execute reported a failed gate without a non-zero command exit",
                details,
            ),
            "gate_execute",
            receipt.artifact_ref.as_deref(),
        );
        let mut report = non_execution_report(request, &error);
        report.setup = setup_attempts;
        return Ok(report);
    }
    let mut report = receipt.report;
    report.setup = setup_attempts;
    // The step is an in-memory compatibility view and is not on the wire.
    // Rebuild it from the verified terminal result, never its serde skip default.
    rebuild_terminal_step(&mut report);
    if let Some(disposition) = report.lab_receipt.as_mut().filter(|value| {
        value.get("schema").and_then(serde_json::Value::as_str) == Some(LAB_GATE_DISPOSITION_SCHEMA)
    }) {
        disposition["artifact_ref"] = serde_json::json!(receipt.artifact_ref);
        return Ok(report);
    }
    report.lab_receipt = Some(serde_json::json!({
        "schema": receipt.schema,
        "request_sha256": receipt.request_sha256,
        "candidate": receipt.candidate,
        "execution_context": receipt.execution_context,
        "artifact_ref": receipt.artifact_ref,
        "materialization": receipt.materialization,
    }));
    Ok(report)
}

/// Type a wrapper/receipt error as the retryable non-execution of its stage.
/// A cancelled disposition stays cancelled; a missing stage is named.
fn gate_execute_unavailable(mut error: Error, stage: &str, artifact_ref: Option<&str>) -> Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    if error
        .details
        .get("stage")
        .is_none_or(serde_json::Value::is_null)
    {
        error.details["stage"] = serde_json::json!(stage);
    }
    if error
        .details
        .get("gate_disposition")
        .is_none_or(serde_json::Value::is_null)
    {
        error.details["gate_disposition"] = serde_json::json!("unavailable");
    }
    if let Some(artifact_ref) = artifact_ref {
        error.details["artifact_ref"] = serde_json::json!(artifact_ref);
    }
    error.retryable.get_or_insert(true);
    error
}

/// The typed cause `gate-execute` reports with its non-zero exit, so it never
/// exits without one (#17649): the failed command with its exit code and
/// output tail, the non-execution stage and cause, or `internal` with the
/// stderr tail. `None` when the receipt passes.
pub fn gate_execute_failure(receipt: &LabGateReceipt) -> Option<serde_json::Value> {
    let report = &receipt.report;
    let ready = receipt
        .readiness
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("ready");
    if report.status == AgentTaskGateStatus::Succeeded || ready {
        return None;
    }
    let visible = report.visibility != AgentTaskGateVisibility::Private;
    let tail = |text: &str| visible.then(|| text_tail(text, 20));
    let command = visible.then(|| report.command.join(" "));
    let disposition = report.lab_receipt.as_ref().filter(|value| {
        value.get("schema").and_then(serde_json::Value::as_str) == Some(LAB_GATE_DISPOSITION_SCHEMA)
    });
    let failure = if report.status == AgentTaskGateStatus::Failed
        && (report.exit_code != 0 || report.workspace_mutation.is_some())
    {
        serde_json::json!({
            "code": "gate.failed",
            "message": format!("deterministic gate {} exited {}", report.id, report.exit_code),
            "details": {"stage": "gate_command", "command": command, "exit_code": report.exit_code},
            "stdout_tail": tail(&report.stdout),
            "stderr_tail": tail(&report.stderr),
            "retryable": false,
        })
    } else if let Some(disposition) = disposition {
        serde_json::json!({
            "code": disposition.get("error_code"),
            "message": disposition.pointer("/diagnostic/message").and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("deterministic gate {} did not execute", report.id)),
            "details": {"stage": disposition.get("stage"), "gate_disposition": disposition.get("disposition"), "command": command},
            "retryable": disposition.get("retryable"),
        })
    } else {
        serde_json::json!({
            "code": homeboy_core::ErrorCode::InternalUnexpected.as_str(),
            "message": format!("gate-execute ended without a gate result (gate status {:?})", report.status),
            "details": {"stage": "gate_execute", "command": command},
            "stderr_tail": tail(&report.stderr),
            "retryable": true,
        })
    };
    Some(homeboy_core::redaction::redact_json(&failure))
}

pub(super) fn rebuild_terminal_step(report: &mut AgentTaskGateReport) {
    report.step = PlanStep::builder(
        report.id.clone(),
        "agent_task.gate",
        match report.status {
            AgentTaskGateStatus::Succeeded => PlanStepStatus::Success,
            AgentTaskGateStatus::Failed | AgentTaskGateStatus::AcceptedInheritedFailure => {
                PlanStepStatus::Failed
            }
            AgentTaskGateStatus::Skipped
            | AgentTaskGateStatus::Deferred
            | AgentTaskGateStatus::Unavailable => PlanStepStatus::Skipped,
        },
    )
    .gate_result(HomeboyGateResult::from(report.clone()))
    .build();
}

/// A gate that ran but left tracked candidate content changed is a failed
/// gate, never a pass and never a transport failure: the receipt keeps the
/// real exit code, termination and output tails, names the rewritten paths
/// first in stderr and carries the typed mutation record (#16920).
fn fail_for_workspace_mutation(
    report: &mut AgentTaskGateReport,
    command: &str,
    mutation: AgentTaskGateWorkspaceMutation,
) {
    let paths = if mutation.differing_paths.is_empty() {
        match &mutation.path_comparison_error {
            Some(cause) => format!("unnamed paths ({cause})"),
            None => format!(
                "tree {} became {}",
                mutation.expected_tree, mutation.actual_tree
            ),
        }
    } else {
        mutation.differing_paths.join(", ")
    };
    let explanation = format!(
        "the gate modified tracked files: {paths}; Lab gates must leave the candidate tree unchanged"
    );
    report.stderr = if report.stderr.is_empty() {
        explanation.clone()
    } else {
        format!("{explanation}\n{}", report.stderr)
    };
    report.status = AgentTaskGateStatus::Failed;
    if report.test_execution_outcome
        == Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Passed)
    {
        report.test_execution_outcome =
            Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Failed);
    }
    let feedback = format!(
        "The gate `{command}` rewrote tracked candidate files ({paths}). Lab gates must leave the candidate tree unchanged: commit the regenerated output in the candidate (for example a rebuilt `dist/`) or change the gate so it does not write tracked files."
    );
    match report.failure_evidence.as_mut() {
        Some(evidence) => {
            evidence.summary = format!("{explanation}; {}", evidence.summary);
            evidence.stderr_tail = text_tail(&report.stderr, 20);
            evidence.agent_feedback = format!("{feedback} {}", evidence.agent_feedback);
        }
        None => {
            report.failure_evidence = Some(AgentTaskGateFailureEvidence {
                classification: AgentTaskGateFailureClassification::CandidateCode,
                summary: format!("deterministic gate failed: {explanation}: {command}"),
                command: command.to_string(),
                exit_code: report.exit_code,
                stdout_tail: text_tail(&report.stdout, 20),
                stderr_tail: text_tail(&report.stderr, 20),
                agent_feedback: feedback,
                diagnostics: Vec::new(),
            });
        }
    }
    report.workspace_mutation = Some(mutation);
    rebuild_terminal_step(report);
}

fn non_execution_report(request: &LabGateRequest, error: &Error) -> AgentTaskGateReport {
    gate_non_execution_report(
        request.index,
        request.argv.clone(),
        request.visibility,
        request.reveal_policy,
        error,
    )
}

/// A Lab gate that never executed has no verdict on the candidate: it is
/// `Unavailable` (promotion `GateDeferred`), carries no exit code and no
/// candidate-code failure evidence, and names its typed cause (#16932).
pub(crate) fn gate_non_execution_report(
    index: usize,
    argv: Vec<String>,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    error: &Error,
) -> AgentTaskGateReport {
    let disposition = error
        .details
        .get("gate_disposition")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unavailable");
    let diagnostic = homeboy_core::redaction::redact_json(&serde_json::json!({
        "code": error.code.as_str(),
        "message": error.message,
        "details": error.details,
    }));
    let message = format!(
        "Gate unavailable: setup or execution infrastructure did not execute the gate: [{}] {}",
        error.code.as_str(),
        diagnostic["message"].as_str().unwrap_or("unknown cause"),
    );
    // Exit code 0 is the not-executed representation `Skipped` uses; the
    // status, never the exit code, carries the outcome.
    let mut report = AgentTaskGateReport::new(
        format!("gate-{index}"),
        argv,
        0,
        "",
        &message,
        None,
        visibility,
        reveal_policy,
        AgentTaskGateEnvironment::default(),
    );
    report.status = AgentTaskGateStatus::Unavailable;
    if disposition == "cancelled" {
        report.termination = AgentTaskGateTermination::Cancelled;
    }
    report.lab_receipt = Some(serde_json::json!({
        "schema": LAB_GATE_DISPOSITION_SCHEMA,
        "disposition": disposition,
        "executed": false,
        "error_code": error.code.as_str(),
        "stage": error.details.get("stage"),
        "job_id": error.details.get("job_id"),
        "retryable": error.retryable,
        "artifact_ref": error.details.get("artifact_ref"),
        "diagnostic": diagnostic,
    }));
    rebuild_terminal_step(&mut report);
    report
}

pub(super) fn preflight(
    cwd: &Path,
    runner_id: &str,
    environment: &AgentTaskGateEnvironmentPolicy,
    toolchains: &[AgentTaskGateToolchainRequirement],
    packages: &[AgentTaskGatePackageArtifactRequirement],
    timeout: Duration,
) -> Result<()> {
    let request = LabGateRequest {
        schema: LabGateRequestSchema::V2,
        preflight_only: true,
        toolchains: toolchains.to_vec(),
        runner_id: runner_id.to_string(),
        candidate: candidate_fingerprint(&cwd.display().to_string())?,
        index: 0,
        argv: vec![],
        label: String::new(),
        visibility: AgentTaskGateVisibility::Visible,
        reveal_policy: AgentTaskGateRevealPolicy::FullEvidence,
        environment: environment.clone(),
        package_artifacts: packages.to_vec(),
        declared_plan: None,
        timeout_seconds: timeout.as_secs().max(1),
        no_progress_timeout_seconds: timeout.as_secs().max(1),
    };
    let receipt = active_transport().execute(cwd, &request, None)?;
    request.verify_receipt(&receipt)?;
    if receipt
        .readiness
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        != Some("ready")
        || receipt.report.status != AgentTaskGateStatus::Skipped
    {
        // The runner reports why it is not ready (a failed probe, a missing
        // package artifact) as a redacted diagnostic on the receipt. Carry it
        // so the operator sees the cause rather than only "not ready".
        let diagnostic = receipt
            .report
            .lab_receipt
            .as_ref()
            .and_then(|lab_receipt| lab_receipt.get("diagnostic"))
            .cloned();
        let cause = diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.get("message"))
            .and_then(serde_json::Value::as_str)
            .map(|message| format!(": {message}"))
            .unwrap_or_default();
        return Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            format!("runner did not return a ready non-execution readiness receipt{cause}"),
            serde_json::json!({"gate_disposition":"unavailable", "stage":"readiness", "artifact_ref":receipt.artifact_ref, "diagnostic":diagnostic}),
        ));
    }
    Ok(())
}

/// Called by the admitted runner command while its verified execution context
/// still resolves against the runner-owned job store, before private HOME.
pub fn execute_admitted(request: LabGateRequest, cwd: &Path) -> Result<LabGateReceipt> {
    execute_admitted_with_resources(request, cwd, None)
}

pub fn execute_admitted_with_resources(
    request: LabGateRequest,
    cwd: &Path,
    resources: Option<Vec<AgentTaskGateExtensionInput>>,
) -> Result<LabGateReceipt> {
    let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::from_direct_daemon_child_environment(&request.runner_id)?
        .ok_or_else(|| Error::invalid_argument("gate.admission", "Lab gate requires a live authenticated runner reservation"))?;
    context.verify_integrity()?;
    execute_with_context_and_resources(request, cwd, &context, resources)
}

/// Exercise runner gate execution with an authenticated fixture context.
#[cfg(any(test, feature = "test-support"))]
pub fn execute_with_context(
    request: LabGateRequest,
    cwd: &Path,
    context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
) -> Result<LabGateReceipt> {
    execute_with_context_and_resources(request, cwd, context, None)
}

/// `PATH` for a Lab gate that does not declare one: the Homeboy runtime's
/// directory first, then the runner user's installed tools.
///
/// Installed tools (npm, node, composer, ...) are not credentials. Gate
/// isolation privatizes `HOME`/XDG config and cache, not tool discovery, so
/// the tool `PATH` resolves against the runner's real home (read here, before
/// the gate's `HOME` is isolated) and the runner process `PATH`. A minimal
/// system `PATH` made every Node/Composer gate exit 127 on the candidate and
/// the baseline alike (#15766).
fn lab_gate_path(runtime_id: &str) -> String {
    let runtime = Path::new(runtime_id);
    let runtime_parent = runtime.is_absolute().then(|| runtime.parent()).flatten();
    let home = homeboy_core::paths::home_root().ok();
    let existing_path = std::env::var_os("PATH");
    let tools = homeboy_engine_primitives::command_path::runner_tool_path(
        home.as_deref(),
        existing_path.as_deref(),
    )
    .map(|path| path.to_string_lossy().into_owned())
    .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());
    match runtime_parent {
        Some(parent) => format!("{}:{tools}", parent.display()),
        None => tools,
    }
}

fn execute_with_context_and_resources(
    request: LabGateRequest,
    cwd: &Path,
    context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
    resources: Option<Vec<AgentTaskGateExtensionInput>>,
) -> Result<LabGateReceipt> {
    context.verify_integrity()?;
    if context.is_local() || context.runner_id() != request.runner_id {
        return Err(Error::invalid_argument(
            "gate.admission",
            "Lab gate requires the selected authenticated runner owner",
        ));
    }
    request.verify_materialized_candidate(cwd)?;
    let mut environment = request.environment.clone();
    let mut playwright_provenance = None;
    environment.lab_runner = None;
    // Runner credentials and its operator registry are not gate inputs. Lab
    // gates receive declared variables/preserve mappings even when the
    // historical local gate policy inherits the controller environment.
    environment.mode = AgentTaskGateEnvironmentMode::Replace;
    if !environment.variables.contains_key("PATH") && !environment.preserve.contains_key("PATH") {
        environment
            .variables
            .insert("PATH".to_string(), lab_gate_path(context.runtime_id()));
    }
    // Isolated HOME/XDG hides the runner's installed browsers, and Replace
    // mode drops its PLAYWRIGHT_BROWSERS_PATH. Like PATH above, map the
    // runner's existing browser cache when the gate declares none; an explicit
    // declaration wins and an absent cache maps nothing.
    if !environment
        .variables
        .contains_key("PLAYWRIGHT_BROWSERS_PATH")
        && !environment
            .preserve
            .contains_key("PLAYWRIGHT_BROWSERS_PATH")
    {
        let declared_playwright_cache =
            std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").map(PathBuf::from);
        let host_home = homeboy_core::paths::home_root().ok();
        let mut values = BTreeMap::new();
        let mut report = AgentTaskGateEnvironment::default();
        preserve_playwright_cache_mapping(
            declared_playwright_cache.as_deref(),
            host_home.as_deref(),
            &mut values,
            &mut report,
        );
        environment.variables.extend(values);
        playwright_provenance = report.preserved.into_iter().next();
        // Resolved paths are variables, not preserve-source mappings.
    }
    if let Some(resources) = resources {
        if resources.len() != environment.extension_inputs.len()
            || resources.iter().zip(&environment.extension_inputs).any(
                |(materialized, declared)| {
                    materialized.id != declared.id
                        || materialized.identity.is_none()
                        || declared.identity.as_ref().is_some_and(|identity| {
                            materialized.identity.as_ref() != Some(identity)
                        })
                },
            )
        {
            return Err(Error::invalid_argument(
                "gate.resources",
                "materialized gate extensions differ from the declared closure",
            ));
        }
        environment.extension_inputs = resources;
    }
    // The existing subprocess projection is emitted only after authenticating
    // the accepted runner context. Nested Homeboy gates execute in place rather
    // than attempting admission through their deliberately private registry.
    environment.variables.insert(
        homeboy_runner_contract::RUNNER_HOSTED_EXEC_ENV.to_string(),
        "1".to_string(),
    );
    environment.variables.insert(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::json!({"schema": "homeboy/lab-offload-subprocess/v1", "runner_id": context.runner_id(), "runner_job_execution_context": context.evidence_record()?}).to_string());
    if !environment.isolate_home || !environment.isolate_xdg {
        return Err(Error::invalid_argument(
            "gate.environment",
            "Lab gates require private HOME and XDG directories",
        ));
    }
    // One deadline per Lab gate (#16938): dependency setup gets at most the
    // whole budget and the gate command only what setup left, so one
    // gate-execute ends within the controller's single wait for its receipt.
    let budget = LabGateBudget::start(&request);
    let supervision = GateSupervision {
        timeout: budget.limit,
        no_progress_timeout: budget.no_progress_limit,
        heartbeat_interval: Duration::from_secs(5),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
    let invocation =
        homeboy_core::engine::invocation::InvocationGuard::acquire(&run_dir, &Default::default())?;
    // Dependency setup runs here, in the exact workspace and environment the
    // gate is about to execute in, so a gate that needs installed dependencies
    // finds them (#16262). The controller never hydrates its own checkout for
    // Lab placement; this receipt carries the setup evidence instead.
    let setup = if request.preflight_only || !environment.hydrate_dependencies {
        Vec::new()
    } else {
        match hydrate_lab_gate_workspace(
            cwd,
            &environment,
            &supervision,
            budget.remaining(),
            &request,
        ) {
            Ok(mut setup) => {
                budget.name_spent_setup(&mut setup);
                setup
            }
            Err(error) => {
                run_dir.finish(false);
                return failed_lab_gate_setup_receipt(&request, context, Vec::new(), Some(error));
            }
        }
    };
    if let Some(failed) = setup
        .iter()
        .find(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed)
    {
        // A failed setup must not run the gate: the receipt stays non-execution
        // evidence with the recorded outcome, and the controller surfaces it as
        // a gate-setup failure rather than candidate blame (#16262).
        let diagnostic = homeboy_core::redaction::redact_json(&serde_json::json!({
            "message": format!(
                "dependency setup failed: provider {} on package root {} ({})",
                failed.provider_id, failed.package_root, failed.reason,
            ),
            "outcome": failed,
        }));
        run_dir.finish(false);
        return failed_lab_gate_setup_receipt(&request, context, setup, None).map(|mut receipt| {
            receipt
                .report
                .lab_receipt
                .as_mut()
                .expect("non-execution receipt")["diagnostic"] = diagnostic;
            receipt
        });
    }
    let setup_elapsed = budget.elapsed();
    let result = if request.preflight_only {
        preflight_gate_toolchains_local(
            cwd,
            &environment,
            &request.toolchains,
            &request.package_artifacts,
            Some(&invocation.context().tmp_dir),
            supervision.timeout,
            true,
        )
        .map(|()| {
            AgentTaskGateReport::skipped(
                format!("gate-{}", request.index),
                vec![],
                request.visibility,
                request.reveal_policy,
                "readiness-only",
            )
        })
    } else if budget.remaining() < LAB_GATE_MIN_COMMAND_BUDGET {
        // Setup finished but left no usable time: the command never started,
        // so this is unavailable environment, not a candidate verdict.
        Err(budget.exhausted_before_command_error())
    } else {
        let started = Instant::now();
        run_gate_argv_local(
            cwd,
            request.index,
            request.argv.clone(),
            &request.label,
            request.visibility,
            request.reveal_policy,
            GateExecution {
                runtime_tmpdir: Some(&invocation.context().tmp_dir),
                timeout: Some(budget.remaining()),
                supervision: Some(&supervision),
                baseline_timeout_diagnostic: false,
                deadline: Some(budget.deadline),
            },
            &environment,
            &request.package_artifacts,
            request.declared_plan.as_ref(),
        )
        .map(|mut report| {
            budget.name_limit(&mut report, started.elapsed(), setup_elapsed);
            report
        })
    };
    run_dir.finish(
        matches!(&result, Ok(report) if report.status == AgentTaskGateStatus::Succeeded
        || (request.preflight_only && report.status == AgentTaskGateStatus::Skipped)),
    );
    let (mut report, readiness) = match result {
        Ok(report) => (report, request.preflight_only.then(|| serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"ready", "executed_gate":false, "toolchains":request.toolchains}))),
        Err(error) => {
            let mut setup = error.clone();
            setup.details["gate_disposition"] = serde_json::json!("unavailable");
            let mut report = non_execution_report(&request, &setup);
            report.lab_receipt.as_mut().expect("non-execution receipt")["diagnostic"] = homeboy_core::redaction::redact_json(&serde_json::json!({"code":error.code.as_str(), "message":error.message, "details":error.details}));
            (report, Some(serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"unavailable", "executed_gate":false})))
        }
    };
    if let Some(plan) = &request.declared_plan {
        report.invocation = Some(AgentTaskGateInvocation::DeclaredTest { plan: plan.clone() });
    }
    report.environment.lab_runner = Some(request.runner_id.clone());
    report.setup = setup.clone();
    if let Some((name, source)) = playwright_provenance {
        report.environment.preserved.insert(name, source);
    }
    // The gate already ran: a changed tree fails it with its evidence intact.
    // Without an executed gate (or a typed tree diff) the mismatch stays fatal.
    if let Some(mismatch) = request.materialized_candidate_mismatch(cwd)? {
        match mismatch.mutation {
            Some(mutation) if readiness.is_none() => {
                fail_for_workspace_mutation(&mut report, &request.label, mutation)
            }
            _ => return Err(mismatch.error),
        }
    }
    Ok(LabGateReceipt {
        readiness,
        materialization: None,
        artifact_ref: None,
        schema: LAB_GATE_RECEIPT_SCHEMA.to_string(),
        request_sha256: request.identity()?,
        candidate: request.candidate,
        execution_context: context.evidence_record()?,
        report,
        setup,
    })
}

/// Smallest remainder worth starting the gate command with. Less than this
/// after dependency setup is reported as a timed-out gate that never ran.
const LAB_GATE_MIN_COMMAND_BUDGET: Duration = Duration::from_secs(1);

/// The single wall-clock budget of one runner-side Lab gate: dependency setup
/// and the gate command both draw from `request.timeout_seconds`.
struct LabGateBudget {
    started: Instant,
    deadline: Instant,
    limit: Duration,
    no_progress_limit: Duration,
}

impl LabGateBudget {
    fn start(request: &LabGateRequest) -> Self {
        let started = Instant::now();
        let limit = Duration::from_secs(request.timeout_seconds.max(1));
        Self {
            started,
            deadline: started + limit,
            limit,
            no_progress_limit: Duration::from_secs(request.no_progress_timeout_seconds.max(1)),
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// A setup step that hit the deadline spent the gate's limit: say so, with
    /// the elapsed time and the limit, in the outcome the controller reports.
    fn name_spent_setup(&self, setup: &mut [AgentTaskGateSetupEvidence]) {
        for outcome in setup.iter_mut().filter(|outcome| {
            outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed
                && outcome.termination
                    == homeboy_core::deps::DependencyHydrationTermination::TimedOut
        }) {
            outcome.reason = format!(
                "the Lab gate time limit of {}s was spent in dependency setup ({}s elapsed)",
                self.limit.as_secs(),
                self.elapsed().as_secs(),
            );
        }
    }

    /// One line naming the effective Lab limits on a supervisor-stopped gate.
    fn limit_note(
        &self,
        termination: AgentTaskGateTermination,
        gate_elapsed: Duration,
        setup_elapsed: Duration,
    ) -> Option<String> {
        let limit = self.limit.as_secs();
        let setup = setup_elapsed.as_secs();
        let elapsed = gate_elapsed.as_secs();
        match termination {
            AgentTaskGateTermination::TimedOut => Some(format!(
                "gate timed out after {elapsed}s (Lab gate limit {limit}s, {setup}s of it spent in dependency setup)"
            )),
            AgentTaskGateTermination::NoProgress => Some(format!(
                "gate stopped after {elapsed}s without progress (no-progress limit {}s, Lab gate limit {limit}s)",
                self.no_progress_limit.as_secs()
            )),
            _ => None,
        }
    }

    fn name_limit(
        &self,
        report: &mut AgentTaskGateReport,
        gate_elapsed: Duration,
        setup_elapsed: Duration,
    ) {
        let Some(note) = self.limit_note(report.termination, gate_elapsed, setup_elapsed) else {
            return;
        };
        report.stderr = if report.stderr.is_empty() {
            note.clone()
        } else {
            format!("{}\n{note}", report.stderr)
        };
        if let Some(evidence) = report.failure_evidence.as_mut() {
            evidence.summary = format!("{}; {note}", evidence.summary);
        }
    }

    /// Setup left less than [`LAB_GATE_MIN_COMMAND_BUDGET`]: the command never
    /// started, so the gate is unavailable with the spent budget as its cause.
    fn exhausted_before_command_error(&self) -> Error {
        let note = self
            .limit_note(
                AgentTaskGateTermination::TimedOut,
                Duration::ZERO,
                self.elapsed(),
            )
            .expect("timed-out note");
        let mut error = Error::new(
            homeboy_core::ErrorCode::DependencyStepFailed,
            format!("dependency setup left no time to run the gate; {note}"),
            serde_json::json!({"stage":"dependency_setup"}),
        );
        error.retryable = Some(true);
        error
    }
}

/// Hydrate the provider-declared dependency roots of the materialized Lab gate
/// workspace, under the runner-resolved gate environment. This is the same
/// primitive the local promotion path uses; the Lab runner merely executes it
/// on the workspace the gate will actually run in (#16262).
fn hydrate_lab_gate_workspace(
    cwd: &Path,
    environment: &AgentTaskGateEnvironmentPolicy,
    supervision: &GateSupervision,
    timeout: Duration,
    request: &LabGateRequest,
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    let hydration = GateHydrationEnvironment::prepare(environment)?;
    let policy = homeboy_core::deps::DependencyHydrationPolicy {
        timeout,
        no_progress_timeout: supervision.no_progress_timeout,
        heartbeat_interval: supervision.heartbeat_interval,
        is_cancelled: Arc::new(|| false),
        on_progress: Arc::new(|_| {}),
        environment: hydration.environment(),
    };
    let setup = hydrate_gate_dependency_roots_for_component(
        cwd,
        environment.hydrate_dependencies,
        "lab_gate_workspace",
        &policy,
        request.environment.admitted_component_id.as_deref(),
        Some(&[request.argv.join(" ")]),
    );
    hydration.finish(setup.as_ref().is_ok_and(|setup| {
        setup
            .iter()
            .all(|outcome| outcome.status != homeboy_core::deps::DependencyHydrationStatus::Failed)
    }));
    setup
}

/// Terminal receipt for a gate whose runner-side dependency setup failed
/// before the gate command could run. The gate never executes, so the receipt
/// stays non-execution evidence (never candidate blame); the controller maps
/// the recorded outcome to a promotion gate-setup failure (#16262).
fn failed_lab_gate_setup_receipt(
    request: &LabGateRequest,
    context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
    setup: Vec<AgentTaskGateSetupEvidence>,
    error: Option<Error>,
) -> Result<LabGateReceipt> {
    let mut setup_error = match error {
        Some(error) => error,
        None => Error::new(
            homeboy_core::ErrorCode::DependencyStepFailed,
            "Lab gate dependency setup failed before the gate command could run",
            serde_json::Value::Object(serde_json::Map::new()),
        ),
    };
    setup_error.details["gate_disposition"] = serde_json::json!("unavailable");
    setup_error.details["stage"] = serde_json::json!("dependency_setup");
    let mut report = non_execution_report(request, &setup_error);
    report.environment.lab_runner = Some(request.runner_id.clone());
    report.lab_receipt.as_mut().expect("non-execution receipt")["diagnostic"] =
        homeboy_core::redaction::redact_json(&serde_json::json!({
            "code": setup_error.code.as_str(),
            "message": setup_error.message,
            "details": setup_error.details,
        }));
    report.setup = setup.clone();
    Ok(LabGateReceipt {
        readiness: Some(
            serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"unavailable", "executed_gate":false}),
        ),
        materialization: None,
        artifact_ref: None,
        schema: LAB_GATE_RECEIPT_SCHEMA.to_string(),
        request_sha256: request.identity()?,
        candidate: request.candidate.clone(),
        execution_context: context.evidence_record()?,
        report,
        setup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_execution_diagnostic_preserves_cause_code_and_redacted_details() {
        let error = Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "runner receipt stream was truncated",
            serde_json::json!({"stage":"terminal_projection", "stdout_bytes":9000000,
                "gate_disposition":"unavailable", "password":"do-not-publish"}),
        );
        let report = gate_non_execution_report(
            1,
            vec!["test".into()],
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            &error,
        );
        assert!(report.stderr.contains(&error.message));
        assert!(report.stderr.contains(error.code.as_str()));
        let receipt = report.lab_receipt.unwrap();
        assert_eq!(receipt["executed"], false);
        assert_eq!(receipt["diagnostic"]["details"]["stdout_bytes"], 9000000);
        assert!(!receipt.to_string().contains("do-not-publish"));
        assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
    }

    /// Transport, terminal-projection, lost-observation and wait-timeout
    /// failures never executed the gate: they are `Unavailable` with no exit
    /// code and no candidate-code evidence, never `Failed` (#16932).
    #[test]
    fn every_non_execution_is_unavailable_with_its_typed_cause() {
        for (disposition, private) in [(None, false), (Some("cancelled"), true)] {
            let mut details = serde_json::json!({"stage":"terminal_projection",
                "job_id":"job-7", "artifact_ref":"homeboy://run/r/artifact/a"});
            if let Some(disposition) = disposition {
                details["gate_disposition"] = serde_json::json!(disposition);
            }
            let mut error = Error::new(
                homeboy_core::ErrorCode::RunnerLabTransportFailure,
                "runner gate did not return terminal artifact evidence",
                details,
            );
            error.retryable = Some(true);
            let (visibility, reveal_policy) = if private {
                (
                    AgentTaskGateVisibility::Private,
                    AgentTaskGateRevealPolicy::SummaryOnly,
                )
            } else {
                (
                    AgentTaskGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                )
            };
            let report = gate_non_execution_report(
                2,
                vec!["sh".into(), "-c".into(), "secret-check".into()],
                visibility,
                reveal_policy,
                &error,
            );

            assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
            assert_eq!(report.exit_code, 0, "a gate that never ran has no exit");
            assert!(report.failure_evidence.is_none());
            assert_eq!(report.step.status, PlanStepStatus::Skipped);
            assert_eq!(
                report.termination == AgentTaskGateTermination::Cancelled,
                disposition == Some("cancelled")
            );
            let result = HomeboyGateResult::from(report.clone());
            assert_eq!(result.status, HomeboyGateStatus::Blocked);
            assert!(!result.summary.contains("passed"), "{}", result.summary);
            assert!(result.summary.contains(
                "Lab gate did not execute: [runner.lab_transport_failure] stage terminal_projection, job job-7; retryable; diagnostic homeboy://run/r/artifact/a"
            ), "{}", result.summary);
            assert!(result.summary.contains("homeboy agent-task recover"));
            assert!(
                !result.summary.contains("secret-check"),
                "the summary names the cause, never the gate command or output"
            );
            assert_eq!(report.lab_receipt.as_ref().unwrap()["retryable"], true);
        }
    }

    #[test]
    fn canonical_closure_comparison_checks_terminal_shared_evidence_and_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(source.join("fixture")).unwrap();
        fs::create_dir_all(source.join("shared")).unwrap();
        fs::write(source.join("fixture/fixture.json"), "{}").unwrap();
        fs::write(source.join("shared/declared.txt"), "declared").unwrap();
        fs::write(
            source.join("homeboy-extension-root.json"),
            r#"{"shared_assets":["shared"]}"#,
        )
        .unwrap();
        let (_, expected) = seal_extension_resources(
            &root.path().join("sealed"),
            &[AgentTaskGateExtensionInput {
                id: "fixture".to_string(),
                source: source.join("fixture").display().to_string(),
                identity: None,
            }],
        )
        .unwrap();
        assert_eq!(expected[0].shared_assets.len(), 1);
        let mut transported = expected.clone();
        transported[0].source = "/runner/package/fixture".to_string();
        transported[0].shared_assets[0].source = "/runner/package/shared".to_string();
        verify_extension_resource_closure(&expected, &transported).unwrap();
        let mut altered = transported.clone();
        altered[0].shared_assets[0].identity = "altered-terminal-shared-tree".to_string();
        assert_eq!(
            altered[0].identity, expected[0].identity,
            "test keeps the extension-level claim unchanged"
        );
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered[0].shared_assets.clear();
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered[0].shared_assets[0].destination = ".config/homeboy/undeclared".to_string();
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        altered = transported.clone();
        altered.push(transported[0].clone());
        assert!(verify_extension_resource_closure(&expected, &altered).is_err());
        fs::write(source.join("shared/declared.txt"), "changed on controller").unwrap();
        let (_, changed) = seal_extension_resources(
            &root.path().join("revalidated"),
            &[AgentTaskGateExtensionInput {
                id: "fixture".to_string(),
                source: source.join("fixture").display().to_string(),
                identity: None,
            }],
        )
        .unwrap();
        assert_eq!(
            fs::read(source.join("fixture/fixture.json")).unwrap(),
            b"{}"
        );
        assert_ne!(
            expected[0].shared_assets[0].identity,
            changed[0].shared_assets[0].identity
        );
        assert!(verify_extension_resource_closure(&expected, &changed).is_err());
    }

    fn repository(root: &Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        fs::write(root.join("candidate.txt"), "candidate\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "candidate"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    fn request(root: &Path) -> LabGateRequest {
        LabGateRequest {
            schema: LabGateRequestSchema::V2,
            preflight_only: false,
            toolchains: vec![],
            runner_id: "lab".to_string(),
            candidate: candidate_fingerprint(&root.display().to_string()).unwrap(),
            index: 1,
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "test -f candidate.txt && test ! -e \"$HOME/operator-marker\" && test -z \"${UNDECLARED_OPERATOR_CREDENTIAL:-}\" && printf executed"
                    .to_string(),
            ],
            label: "candidate gate".to_string(),
            visibility: AgentTaskGateVisibility::Visible,
            reveal_policy: AgentTaskGateRevealPolicy::FullEvidence,
            environment: AgentTaskGateEnvironmentPolicy {
                hydrate_rust_cache: false,
                mode: AgentTaskGateEnvironmentMode::Inherit,
                ..Default::default()
            },
            package_artifacts: vec![],
            declared_plan: None,
            timeout_seconds: 30,
            no_progress_timeout_seconds: 30,
        }
    }

    #[test]
    fn request_schema_rejects_skewed_runners() {
        let temp = tempfile::tempdir().unwrap();
        let mut value = serde_json::to_value(request(temp.path())).unwrap();
        assert_eq!(value["schema"], "homeboy/agent-task-lab-gate-request/v2");
        serde_json::from_value::<LabGateRequest>(value.clone()).unwrap();
        value["schema"] = serde_json::json!("homeboy/agent-task-lab-gate-request/v1");
        assert!(serde_json::from_value::<LabGateRequest>(value).is_err());
    }

    #[test]
    fn terminal_receipt_binds_execution_and_rejects_changed_candidate_and_invocation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _credential = homeboy_core::test_support::EnvVarGuard::set(
                "UNDECLARED_OPERATOR_CREDENTIAL",
                "fixture-secret",
            );
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = request(temp.path());
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();
            let receipt = execute_with_context(request.clone(), temp.path(), &context).unwrap();
            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(receipt.report.stdout, "executed");
            assert_eq!(
                receipt.report.environment.mode,
                AgentTaskGateEnvironmentMode::Replace
            );
            assert!(receipt
                .report
                .environment
                .sanitized
                .iter()
                .any(|value| value.name == "HOME"));
            request.verify_receipt(&receipt).unwrap();
            let mut altered = request.clone();
            altered.argv.push("altered".to_string());
            assert!(altered.verify_receipt(&receipt).is_err());
            let mut wrong_owner = receipt.clone();
            wrong_owner.execution_context =
                homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::local(
                    "homeboy",
                )
                .evidence_record()
                .unwrap();
            assert!(request.verify_receipt(&wrong_owner).is_err());
            fs::write(temp.path().join("candidate.txt"), "altered candidate\n").unwrap();
            assert!(request.verify_materialized_candidate(temp.path()).is_err());
        });
    }

    #[test]
    fn materialized_candidate_mismatch_names_exact_tree_paths() {
        let temp = tempfile::tempdir().unwrap();
        repository(temp.path());
        // A repository-level checkout deliberately has no root manifest.
        let request = request(temp.path());
        request.verify_materialized_candidate(temp.path()).unwrap();
        fs::remove_file(temp.path().join("candidate.txt")).unwrap();
        fs::create_dir_all(temp.path().join("plugins/example/tests")).unwrap();
        fs::write(temp.path().join("plugins/example/tests/new.txt"), "new").unwrap();
        let error = request
            .verify_materialized_candidate(temp.path())
            .unwrap_err();
        assert_eq!(
            error.details["differing_paths"],
            serde_json::json!(["candidate.txt", "plugins/example/tests/new.txt"])
        );
        assert!(error.message.contains("candidate.txt"));
        assert!(error.message.contains("plugins/example/tests/new.txt"));
        assert_ne!(error.details["expected_tree"], error.details["actual_tree"]);
    }

    #[test]
    fn materialized_candidate_ignores_nested_gitignored_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        repository(temp.path());
        fs::write(temp.path().join(".gitignore"), "vendor/\n").unwrap();
        let request = request(temp.path());
        fs::create_dir_all(temp.path().join("plugins/example/tools/vendor")).unwrap();
        fs::write(
            temp.path()
                .join("plugins/example/tools/vendor/installed.php"),
            "dependency",
        )
        .unwrap();
        request.verify_materialized_candidate(temp.path()).unwrap();
    }

    fn gate_printing_path_and_home(root: &Path) -> LabGateRequest {
        let mut request = request(root);
        request.argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s\\n%s' \"$PATH\" \"$HOME\"".to_string(),
        ];
        request
    }

    #[test]
    fn lab_gate_without_declared_path_resolves_runner_user_tools() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let local_bin = home.path().join(".local/bin");
            let node_bin = home.path().join(".local/opt/node-v24.13.1-linux-x64/bin");
            fs::create_dir_all(&local_bin).unwrap();
            fs::create_dir_all(&node_bin).unwrap();
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = gate_printing_path_and_home(temp.path());
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "/runner/binaries/homeboy-build/homeboy", "reservation").unwrap();

            let receipt = execute_with_context(request, temp.path(), &context).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            let (path, gate_home) = receipt.report.stdout.split_once('\n').unwrap();
            let parts = std::env::split_paths(path).collect::<Vec<_>>();
            assert_eq!(parts[0], Path::new("/runner/binaries/homeboy-build"));
            let position = |entry: &Path| parts.iter().position(|part| part == entry);
            let local = position(&local_bin).expect("runner ~/.local/bin on gate PATH");
            let node = position(&node_bin).expect("runner node bin on gate PATH");
            let system = position(Path::new("/usr/bin")).expect("system bin on gate PATH");
            assert!(local < system && node < system);
            assert_ne!(
                Path::new(gate_home),
                home.path(),
                "gate HOME stays isolated"
            );
        });
    }

    #[test]
    fn lab_gate_preserves_runner_playwright_cache_in_replace_environment() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let cache = home.path().join(PLAYWRIGHT_CACHE_SUBPATH);
            fs::create_dir_all(&cache).unwrap();
            let _cache_env =
                homeboy_core::test_support::EnvVarGuard::unset("PLAYWRIGHT_BROWSERS_PATH");
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let mut request = gate_printing_path_and_home(temp.path());
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$PLAYWRIGHT_BROWSERS_PATH\"".to_string(),
            ];
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();

            let receipt = execute_with_context(request, temp.path(), &context).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(receipt.report.stdout, cache.display().to_string());
            assert_eq!(
                receipt
                    .report
                    .environment
                    .preserved
                    .get("PLAYWRIGHT_BROWSERS_PATH"),
                Some(&format!("HOME/{PLAYWRIGHT_CACHE_SUBPATH}"))
            );
        });
    }

    fn printing_playwright_cache(temp: &Path) -> LabGateRequest {
        let mut request = gate_printing_path_and_home(temp);
        request.argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s|%s' \"${PLAYWRIGHT_BROWSERS_PATH-unset}\" \"$HOME\"".to_string(),
        ];
        request
    }

    fn lab_context() -> homeboy_core::runner_job_execution_context::RunnerJobExecutionContext {
        homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(
            Some("controller-run"),
            "lab",
            "job",
            "homeboy",
            "reservation",
        )
        .unwrap()
    }

    #[test]
    fn lab_gate_maps_homeboy_managed_browser_cache_in_isolated_replace_environment() {
        homeboy_core::test_support::with_isolated_home(|home| {
            // Both runner caches exist; the Homeboy-managed one is preferred.
            let cache = home.path().join(".cache/homeboy/playwright-browsers");
            fs::create_dir_all(&cache).unwrap();
            fs::create_dir_all(home.path().join(PLAYWRIGHT_CACHE_SUBPATH)).unwrap();
            let _cache_env =
                homeboy_core::test_support::EnvVarGuard::unset("PLAYWRIGHT_BROWSERS_PATH");
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = printing_playwright_cache(temp.path());

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(
                receipt.report.environment.mode,
                AgentTaskGateEnvironmentMode::Replace
            );
            let (browsers, gate_home) = receipt.report.stdout.split_once('|').unwrap();
            assert_eq!(browsers, cache.display().to_string());
            assert_ne!(
                Path::new(gate_home),
                home.path(),
                "gate HOME stays isolated"
            );
            assert_eq!(
                receipt
                    .report
                    .environment
                    .preserved
                    .get("PLAYWRIGHT_BROWSERS_PATH")
                    .map(String::as_str),
                Some("HOME/.cache/homeboy/playwright-browsers")
            );
        });
    }

    #[test]
    fn lab_gate_explicit_playwright_declaration_wins_over_runner_cache() {
        homeboy_core::test_support::with_isolated_home(|home| {
            fs::create_dir_all(home.path().join(PLAYWRIGHT_CACHE_SUBPATH)).unwrap();
            let runner_cache = tempfile::tempdir().unwrap();
            let _cache_env = homeboy_core::test_support::EnvVarGuard::set(
                "PLAYWRIGHT_BROWSERS_PATH",
                runner_cache.path(),
            );
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let mut request = printing_playwright_cache(temp.path());
            // Even a declared path that does not exist on the runner wins.
            request.environment.variables.insert(
                "PLAYWRIGHT_BROWSERS_PATH".to_string(),
                "/declared/browsers".to_string(),
            );

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            let (browsers, _) = receipt.report.stdout.split_once('|').unwrap();
            assert_eq!(browsers, "/declared/browsers");
            assert!(!receipt
                .report
                .environment
                .preserved
                .contains_key("PLAYWRIGHT_BROWSERS_PATH"));
        });
    }

    #[test]
    fn lab_gate_without_runner_browser_cache_maps_nothing() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let missing = tempfile::tempdir().unwrap();
            let _cache_env = homeboy_core::test_support::EnvVarGuard::set(
                "PLAYWRIGHT_BROWSERS_PATH",
                missing.path().join("absent"),
            );
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = printing_playwright_cache(temp.path());

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            let (browsers, _) = receipt.report.stdout.split_once('|').unwrap();
            assert_eq!(browsers, "unset");
            assert!(!receipt
                .report
                .environment
                .preserved
                .contains_key("PLAYWRIGHT_BROWSERS_PATH"));
        });
    }

    #[test]
    fn lab_gate_uses_runner_process_cache_when_request_does_not_declare_one() {
        homeboy_core::test_support::with_isolated_home(|home| {
            // The runner HOME also has a default cache; the runner's declared
            // cache must still win.
            fs::create_dir_all(home.path().join(PLAYWRIGHT_CACHE_SUBPATH)).unwrap();
            let cache_root = tempfile::tempdir().unwrap();
            let cache = cache_root.path().join("runner-playwright-browsers");
            fs::create_dir_all(&cache).unwrap();
            let _cache_env =
                homeboy_core::test_support::EnvVarGuard::set("PLAYWRIGHT_BROWSERS_PATH", &cache);
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let mut request = gate_printing_path_and_home(temp.path());
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$PLAYWRIGHT_BROWSERS_PATH\"".to_string(),
            ];
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();

            let receipt = execute_with_context(request, temp.path(), &context).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(receipt.report.stdout, cache.display().to_string());
        });
    }

    #[test]
    fn lab_gate_declared_runner_cache_outside_home_is_used() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let cache_root = tempfile::tempdir().unwrap();
            let cache = cache_root.path().join("playwright-browsers");
            fs::create_dir_all(&cache).unwrap();
            let mut request = gate_printing_path_and_home(temp.path());
            request.environment.variables.insert(
                "PLAYWRIGHT_BROWSERS_PATH".to_string(),
                cache.display().to_string(),
            );
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$PLAYWRIGHT_BROWSERS_PATH\"".to_string(),
            ];
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();
            let receipt = execute_with_context(request, temp.path(), &context).unwrap();
            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(receipt.report.stdout, cache.display().to_string());
        });
    }

    #[test]
    fn lab_gate_declared_path_is_kept() {
        homeboy_core::test_support::with_isolated_home(|home| {
            fs::create_dir_all(home.path().join(".local/bin")).unwrap();
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let mut request = gate_printing_path_and_home(temp.path());
            request.environment.variables.insert(
                "PATH".to_string(),
                "/declared/bin:/usr/bin:/bin".to_string(),
            );
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "/runner/binaries/homeboy-build/homeboy", "reservation").unwrap();

            let receipt = execute_with_context(request, temp.path(), &context).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            let (path, _) = receipt.report.stdout.split_once('\n').unwrap();
            assert_eq!(path, "/declared/bin:/usr/bin:/bin");
        });
    }

    #[test]
    fn unverified_wire_context_cannot_execute_a_lab_gate() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let context = homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "lab", "job", "homeboy", "reservation").unwrap();
            let assertion = serde_json::from_value(serde_json::to_value(context).unwrap()).unwrap();
            assert!(execute_with_context(request(temp.path()), temp.path(), &assertion).is_err());
        });
    }

    /// A git workspace whose tracked manifest declares a fixture dependency
    /// provider whose install drops a marker in its package root. The gate
    /// command only passes when that marker is already there, so a green gate
    /// proves setup ran in the gate cwd before the argv.
    fn repository_with_dependency_manifest(root: &Path) {
        repository_with_dependency_install(root, "printf hydrated >> hydration-count");
    }

    fn repository_with_dependency_install(root: &Path, install: &str) {
        repository(root);
        fs::write(
            root.join("homeboy.json"),
            serde_json::json!({"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c",install]}}}}).to_string(),
        )
        .unwrap();
        // Dependency outputs are ignored content, exactly like a real
        // node_modules: the hydrated workspace keeps the candidate fingerprint
        // it was admitted with.
        fs::write(root.join(".gitignore"), "/hydration-count\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "dependency manifest"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    /// #16262: the runner hydrates the materialized Lab gate workspace before
    /// the gate command, and the receipt (and its report) carry setup evidence
    /// naming the runner-side workspace the install ran in.
    #[test]
    fn lab_gate_hydrates_its_runner_workspace_before_the_gate_command() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository_with_dependency_manifest(temp.path());
            let mut request = request(temp.path());
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "test -f hydration-count".to_string(),
            ];

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert!(temp.path().join("hydration-count").is_file());
            assert_eq!(receipt.setup.len(), 1, "{:?}", receipt.setup);
            let setup = &receipt.setup[0];
            assert_eq!(
                setup.status,
                homeboy_core::deps::DependencyHydrationStatus::Succeeded
            );
            assert_eq!(setup.package_root, ".");
            assert_eq!(
                Path::new(&setup.cwd),
                temp.path(),
                "setup evidence names the runner-side gate workspace"
            );
            assert_eq!(receipt.report.setup, receipt.setup);
        });
    }

    /// With the hydration policy disabled, the runner runs no dependency
    /// setup: no install, no setup evidence, and the gate observes the
    /// unhydrated workspace.
    #[test]
    fn lab_gate_with_hydration_disabled_runs_no_dependency_setup() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository_with_dependency_manifest(temp.path());
            let mut request = request(temp.path());
            request.environment.hydrate_dependencies = false;
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "test ! -e hydration-count".to_string(),
            ];

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Succeeded);
            assert!(!temp.path().join("hydration-count").exists());
            assert!(receipt.setup.is_empty());
            assert!(receipt.report.setup.is_empty());
        });
    }

    /// #16938: setup and the gate command share one deadline. Slow setup
    /// leaves the gate only the remainder, and the gate that runs out of it
    /// comes back as a TimedOut report with its tails and the Lab limit,
    /// within about one budget rather than two.
    #[test]
    fn lab_gate_command_gets_only_the_budget_dependency_setup_left() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository_with_dependency_install(
                temp.path(),
                "sleep 1.5; printf hydrated >> hydration-count",
            );
            let mut request = request(temp.path());
            request.timeout_seconds = 3;
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf started; printf 'gate log' >&2; exec sleep 10".to_string(),
            ];

            let started = Instant::now();
            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();
            let elapsed = started.elapsed();

            assert!(
                elapsed < Duration::from_millis(4500),
                "one gate-execute stays within one budget, took {elapsed:?}"
            );
            assert_eq!(
                receipt.setup[0].status,
                homeboy_core::deps::DependencyHydrationStatus::Succeeded
            );
            let report = &receipt.report;
            assert_eq!(report.status, AgentTaskGateStatus::Failed);
            assert_eq!(report.termination, AgentTaskGateTermination::TimedOut);
            assert_eq!(report.stdout, "started");
            assert!(report.stderr.contains("gate log"), "{}", report.stderr);
            assert!(
                report.stderr.contains("(Lab gate limit 3s, ")
                    && report.stderr.contains("s of it spent in dependency setup)"),
                "{}",
                report.stderr
            );
            let evidence = report.failure_evidence.as_ref().unwrap();
            assert_eq!(
                evidence.classification,
                AgentTaskGateFailureClassification::ExecutionBudget
            );
            assert!(
                evidence.summary.contains("Lab gate limit 3s"),
                "{}",
                evidence.summary
            );
            assert_eq!(evidence.stdout_tail, "started");
        });
    }

    /// #16938: setup that spends the whole budget is the typed setup failure
    /// (never a gate run), and it names the elapsed time and the limit.
    #[test]
    fn dependency_setup_exceeding_the_lab_gate_limit_is_a_typed_setup_failure() {
        struct RunnerTransport {
            previous: Arc<dyn LabGateTransport>,
        }
        impl LabGateTransport for RunnerTransport {
            fn declared_runner(&self, command: &str) -> Result<Option<String>> {
                self.previous.declared_runner(command)
            }
            fn execute(
                &self,
                cwd: &Path,
                request: &LabGateRequest,
                supervision: Option<&GateSupervision>,
            ) -> Result<LabGateReceipt> {
                if request.runner_id != "slow-setup-runner" {
                    return self.previous.execute(cwd, request, supervision);
                }
                execute_with_context(request.clone(), cwd, &runner_context("slow-setup-runner"))
            }
        }
        homeboy_core::test_support::with_isolated_home(|_| {
            let previous = active_transport();
            register_lab_gate_transport(Arc::new(RunnerTransport { previous }));
            let temp = tempfile::tempdir().unwrap();
            repository_with_dependency_install(temp.path(), "exec sleep 10");
            let marker = temp.path().join("gate-ran");
            let mut request = request(temp.path());
            request.runner_id = "slow-setup-runner".to_string();
            request.timeout_seconds = 1;
            request.argv = vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("touch '{}'", marker.display()),
            ];

            let started = Instant::now();
            let receipt = execute_with_context(
                request.clone(),
                temp.path(),
                &runner_context("slow-setup-runner"),
            )
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(!marker.exists(), "a spent budget never runs the gate");
            let outcome = &receipt.setup[0];
            assert_eq!(
                outcome.termination,
                homeboy_core::deps::DependencyHydrationTermination::TimedOut
            );
            assert!(
                outcome
                    .reason
                    .starts_with("the Lab gate time limit of 1s was spent in dependency setup ("),
                "{}",
                outcome.reason
            );
            assert_eq!(receipt.report.status, AgentTaskGateStatus::Unavailable);
            let diagnostic = &receipt.report.lab_receipt.as_ref().unwrap()["diagnostic"];
            assert!(diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("Lab gate time limit of 1s"));

            let report = dispatch(temp.path(), &request, None)
                .expect("a spent setup budget is unavailable, not a candidate verdict");
            assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
            assert_eq!(report.setup.len(), 2);
            assert!(!marker.exists());
            let diagnostic = &report.lab_receipt.as_ref().unwrap()["diagnostic"];
            assert_eq!(diagnostic["details"]["step_id"], "promotion.gate_setup");
            assert_eq!(
                diagnostic["details"]["cause"]["retry_action"],
                "retry_dependency_hydration"
            );
            assert!(diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("the Lab gate time limit of 1s was spent in dependency setup"));
        });
    }

    /// #16938/#17649: setup that leaves less than the minimum command budget
    /// never started the command, so the gate is unavailable, not failed.
    #[test]
    fn exhausted_budget_before_the_command_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let request = request(temp.path());
        let budget = LabGateBudget {
            started: Instant::now() - Duration::from_secs(30),
            deadline: Instant::now(),
            limit: Duration::from_secs(30),
            no_progress_limit: Duration::from_secs(30),
        };

        let report = non_execution_report(&request, &budget.exhausted_before_command_error());

        assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
        assert_eq!(report.exit_code, 0);
        assert!(report.failure_evidence.is_none());
        let receipt = report.lab_receipt.as_ref().unwrap();
        assert_eq!(receipt["stage"], "dependency_setup");
        assert_eq!(receipt["retryable"], true);
        assert!(
            report.stderr.contains(
                "gate timed out after 0s (Lab gate limit 30s, 30s of it spent in dependency setup)"
            ),
            "{}",
            report.stderr
        );
    }

    /// Dependency setup gets one warm retry and never supplies a gate verdict.
    #[test]
    fn runner_dependency_setup_retries_then_reports_non_execution_or_runs() {
        struct FailedSetupTransport {
            previous: Arc<dyn LabGateTransport>,
            calls: Arc<std::sync::atomic::AtomicUsize>,
            clears_on_retry: bool,
        }
        impl LabGateTransport for FailedSetupTransport {
            fn declared_runner(&self, command: &str) -> Result<Option<String>> {
                self.previous.declared_runner(command)
            }
            fn execute(
                &self,
                cwd: &Path,
                request: &LabGateRequest,
                supervision: Option<&GateSupervision>,
            ) -> Result<LabGateReceipt> {
                if request.runner_id != "setup-failure-runner" {
                    return self.previous.execute(cwd, request, supervision);
                }
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let recovered = self.clears_on_retry && call > 0;
                let mut report = non_execution_report(
                    request,
                    &Error::new(
                        homeboy_core::ErrorCode::DependencyStepFailed,
                        "Lab gate dependency setup failed before the gate command could run",
                        serde_json::json!({"gate_disposition":"unavailable", "stage":"dependency_setup"}),
                    ),
                );
                report.environment.lab_runner = Some(request.runner_id.clone());
                if recovered {
                    report.status = AgentTaskGateStatus::Succeeded;
                    report.stdout = "gate ran".to_string();
                }
                Ok(LabGateReceipt {
                    readiness: Some(serde_json::json!({"schema":"homeboy/lab-gate-readiness/v1", "status":"unavailable", "executed_gate":false})),
                    materialization: None,
                    artifact_ref: None,
                    schema: LAB_GATE_RECEIPT_SCHEMA.to_string(),
                    request_sha256: request.identity().unwrap(),
                    candidate: request.candidate.clone(),
                    execution_context: homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(Some("controller-run"), "setup-failure-runner", "job", "homeboy", "reservation").unwrap().evidence_record().unwrap(),
                    report,
                    setup: vec![homeboy_core::deps::DependencyHydrationOutcome {
                        schema: "homeboy/dependency-hydration-outcome/v1".to_string(),
                        workspace: "lab_gate_workspace".to_string(),
                        package_root: ".".to_string(),
                        provider_id: "fixture".to_string(),
                        command: vec!["sh".to_string(), "-c".to_string(), "exit 3".to_string()],
                        cwd: "/runner/workspaces/candidate".to_string(),
                        reason: "install_failed".to_string(),
                        download_cache: Default::default(),
                        download_auth: Default::default(),
                        termination: if recovered { homeboy_core::deps::DependencyHydrationTermination::Completed } else { homeboy_core::deps::DependencyHydrationTermination::NoProgress },
                        status: if recovered { homeboy_core::deps::DependencyHydrationStatus::Succeeded } else { homeboy_core::deps::DependencyHydrationStatus::Failed },
                        exit_code: None,
                        stdout: String::new(),
                        stderr: "boom".to_string(),
                        missing_outputs: Vec::new(),
                    }],
                })
            }
        }
        homeboy_core::test_support::with_isolated_home(|_| {
            for clears_on_retry in [false, true] {
                let previous = active_transport();
                let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                register_lab_gate_transport(Arc::new(FailedSetupTransport {
                    previous,
                    calls: calls.clone(),
                    clears_on_retry,
                }));
                let temp = tempfile::tempdir().unwrap();
                repository(temp.path());
                let environment = AgentTaskGateEnvironmentPolicy {
                    lab_runner: Some("setup-failure-runner".to_string()),
                    ..Default::default()
                };

                let report = run_gate_command_with_timeout(
                    temp.path(),
                    1,
                    "true",
                    AgentTaskGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                    temp.path(),
                    Duration::from_secs(30),
                    &environment,
                    &[],
                )
                .expect("setup produces a non-execution report, not candidate failure");
                assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
                assert_eq!(report.setup.len(), 2);
                assert_eq!(
                    report.status,
                    if clears_on_retry {
                        AgentTaskGateStatus::Succeeded
                    } else {
                        AgentTaskGateStatus::Unavailable
                    }
                );
                if clears_on_retry {
                    assert_eq!(report.stdout, "gate ran");
                } else {
                    let diagnostic = &report.lab_receipt.as_ref().unwrap()["diagnostic"];
                    assert_eq!(diagnostic["details"]["step_id"], "promotion.gate_setup");
                    assert_eq!(
                        diagnostic["details"]["cause"]["outcome"]["termination"],
                        "no_progress"
                    );
                    assert!(report.failure_evidence.is_none());
                }
            }
        });
    }

    /// #17725: a transient admission refusal (pressure) waits and the gate
    /// runs once the runner accepts; a disconnected runner defers at once and
    /// names its reasons.
    #[test]
    fn gate_admission_waits_out_pressure_and_defers_disconnected_runners() {
        struct AdmissionTransport {
            previous: Arc<dyn LabGateTransport>,
            admissions: std::sync::Mutex<Vec<LabGateAdmission>>,
            observed: Arc<std::sync::atomic::AtomicUsize>,
            executed: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl LabGateTransport for AdmissionTransport {
            fn declared_runner(&self, command: &str) -> Result<Option<String>> {
                self.previous.declared_runner(command)
            }
            fn admission(&self, runner_id: &str) -> Result<LabGateAdmission> {
                if runner_id != "admission-runner" {
                    return self.previous.admission(runner_id);
                }
                self.observed
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut admissions = self.admissions.lock().unwrap();
                Ok(if admissions.len() > 1 {
                    admissions.remove(0)
                } else {
                    admissions[0].clone()
                })
            }
            fn execute(
                &self,
                cwd: &Path,
                request: &LabGateRequest,
                supervision: Option<&GateSupervision>,
            ) -> Result<LabGateReceipt> {
                if request.runner_id != "admission-runner" {
                    return self.previous.execute(cwd, request, supervision);
                }
                self.executed
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                execute_with_context(request.clone(), cwd, &runner_context(&request.runner_id))
            }
        }
        let refused = |reasons: &[&str], freshness: LabGateFreshness| LabGateAdmission {
            accepts_jobs: false,
            reasons: reasons.iter().map(|reason| reason.to_string()).collect(),
            freshness,
            next_action: None,
        };
        homeboy_core::test_support::with_isolated_home(|_| {
            let cases = [
                (
                    vec![
                        refused(&["pressure"], LabGateFreshness::Fresh),
                        refused(&["pressure"], LabGateFreshness::Fresh),
                        LabGateAdmission::accepting(),
                    ],
                    true,
                ),
                (
                    vec![refused(
                        &["not_connected", "active_jobs_unavailable"],
                        LabGateFreshness::NotObserved,
                    )],
                    false,
                ),
            ];
            for (admissions, runs) in cases {
                let previous = active_transport();
                let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                register_lab_gate_transport(Arc::new(AdmissionTransport {
                    previous,
                    admissions: std::sync::Mutex::new(admissions),
                    observed: observed.clone(),
                    executed: executed.clone(),
                }));
                let temp = tempfile::tempdir().unwrap();
                repository(temp.path());
                let mut request = request(temp.path());
                request.runner_id = "admission-runner".to_string();
                request.argv = vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf executed".to_string(),
                ];

                let report = dispatch(temp.path(), &request, None).unwrap();

                let observed = observed.load(std::sync::atomic::Ordering::SeqCst);
                let executed = executed.load(std::sync::atomic::Ordering::SeqCst);
                if runs {
                    assert_eq!(observed, 3, "pressure is re-observed until accepting");
                    assert_eq!(executed, 1);
                    assert_eq!(report.status, AgentTaskGateStatus::Succeeded, "{report:?}");
                    assert_eq!(report.stdout, "executed");
                } else {
                    assert_eq!(observed, 1, "a disconnected runner is not re-observed");
                    assert_eq!(executed, 0);
                    assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
                    assert!(
                        report.stderr.contains(
                            "cannot accept jobs (reasons: not_connected, active_jobs_unavailable; daemon freshness: not_observed)"
                        ),
                        "{}",
                        report.stderr
                    );
                    let diagnostic = &report.lab_receipt.as_ref().unwrap()["diagnostic"];
                    assert_eq!(diagnostic["details"]["stage"], "admission");
                    assert_eq!(diagnostic["details"]["transient"], false);
                    assert_eq!(
                        diagnostic["details"]["reasons"],
                        serde_json::json!(["not_connected", "active_jobs_unavailable"])
                    );
                    assert_eq!(diagnostic["details"]["freshness"], "not_observed");
                }
            }
        });
    }

    fn mutating_gate(root: &Path, runner_id: &str, script: &str) -> LabGateRequest {
        let mut request = request(root);
        request.runner_id = runner_id.to_string();
        request.argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        request
    }

    fn runner_context(
        runner_id: &str,
    ) -> homeboy_core::runner_job_execution_context::RunnerJobExecutionContext {
        homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(
            Some("controller-run"),
            runner_id,
            "job",
            "homeboy",
            "reservation",
        )
        .unwrap()
    }

    /// #16920: a gate that exits 0 but rewrites a tracked file still returns
    /// the bound receipt, failed, with its real output and the typed record.
    #[test]
    fn passing_gate_that_rewrites_tracked_files_returns_a_failed_bound_receipt() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = mutating_gate(
                temp.path(),
                "lab",
                "printf executed; printf 'gate log\\n' >&2; printf rebuilt > candidate.txt",
            );

            let receipt =
                execute_with_context(request.clone(), temp.path(), &lab_context()).unwrap();

            request.verify_receipt(&receipt).unwrap();
            assert_eq!(receipt.candidate, request.candidate);
            let report = &receipt.report;
            assert_eq!(report.status, AgentTaskGateStatus::Failed);
            assert_eq!(report.exit_code, 0);
            assert_eq!(report.termination, AgentTaskGateTermination::Completed);
            assert_eq!(report.stdout, "executed");
            assert!(
                report.stderr.starts_with(
                    "the gate modified tracked files: candidate.txt; Lab gates must leave the candidate tree unchanged"
                ),
                "{}",
                report.stderr
            );
            assert!(report.stderr.contains("gate log"));
            let mutation = report.workspace_mutation.as_ref().unwrap();
            assert_eq!(mutation.differing_paths, vec!["candidate.txt".to_string()]);
            assert_ne!(mutation.expected_tree, mutation.actual_tree);
            assert_eq!(mutation.path_comparison_error, None);
            let evidence = report.failure_evidence.as_ref().unwrap();
            assert_eq!(
                evidence.classification,
                AgentTaskGateFailureClassification::CandidateCode
            );
            assert!(evidence.agent_feedback.contains("candidate.txt"));
            assert!(evidence.stderr_tail.contains("modified tracked files"));
            let wire = serde_json::to_value(&receipt).unwrap();
            assert_eq!(
                wire["report"]["workspace_mutation"]["differing_paths"],
                serde_json::json!(["candidate.txt"])
            );
            serde_json::from_value::<LabGateReceipt>(wire).unwrap();
        });
    }

    #[test]
    fn failing_gate_that_rewrites_tracked_files_keeps_its_exit_code() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = mutating_gate(
                temp.path(),
                "lab",
                "printf rebuilt > candidate.txt; printf boom >&2; exit 3",
            );

            let receipt = execute_with_context(request, temp.path(), &lab_context()).unwrap();

            assert_eq!(receipt.report.status, AgentTaskGateStatus::Failed);
            assert_eq!(receipt.report.exit_code, 3);
            assert!(receipt
                .report
                .stderr
                .starts_with("the gate modified tracked files: candidate.txt"));
            assert!(receipt.report.stderr.ends_with("boom"));
            assert_eq!(
                receipt.report.failure_evidence.as_ref().unwrap().exit_code,
                3
            );
            assert_eq!(
                receipt
                    .report
                    .workspace_mutation
                    .as_ref()
                    .unwrap()
                    .differing_paths,
                vec!["candidate.txt".to_string()]
            );
        });
    }

    #[test]
    fn pre_run_candidate_mismatch_is_still_refused() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let marker = temp.path().join("gate-ran");
            let request =
                mutating_gate(temp.path(), "lab", &format!("touch '{}'", marker.display()));
            fs::write(temp.path().join("candidate.txt"), "altered before run\n").unwrap();

            let error = execute_with_context(request, temp.path(), &lab_context()).unwrap_err();

            assert_eq!(
                error.details["differing_paths"],
                serde_json::json!(["candidate.txt"])
            );
            assert!(error
                .message
                .contains("differs from the controller candidate"));
            assert!(
                !marker.exists(),
                "a mismatched workspace never runs the gate"
            );
        });
    }

    /// The controller accepts the bound mutation receipt as an executed,
    /// failed gate rather than a transport/non-execution report.
    #[test]
    fn controller_dispatch_reports_workspace_mutation_as_a_failed_gate() {
        struct MutatingRunnerTransport {
            previous: Arc<dyn LabGateTransport>,
        }
        impl LabGateTransport for MutatingRunnerTransport {
            fn declared_runner(&self, command: &str) -> Result<Option<String>> {
                self.previous.declared_runner(command)
            }
            fn execute(
                &self,
                cwd: &Path,
                request: &LabGateRequest,
                supervision: Option<&GateSupervision>,
            ) -> Result<LabGateReceipt> {
                if request.runner_id != "mutating-runner" {
                    return self.previous.execute(cwd, request, supervision);
                }
                // The runner materializes its own copy of the candidate.
                let runner = tempfile::tempdir().unwrap();
                assert!(Command::new("git")
                    .args(["clone", "-q"])
                    .arg(cwd)
                    .arg(runner.path())
                    .status()
                    .unwrap()
                    .success());
                execute_with_context(
                    request.clone(),
                    runner.path(),
                    &runner_context("mutating-runner"),
                )
            }
        }
        homeboy_core::test_support::with_isolated_home(|_| {
            let previous = active_transport();
            register_lab_gate_transport(Arc::new(MutatingRunnerTransport { previous }));
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let request = mutating_gate(
                temp.path(),
                "mutating-runner",
                "printf executed; printf rebuilt > candidate.txt",
            );

            let report = dispatch(temp.path(), &request, None).unwrap();

            assert_eq!(report.status, AgentTaskGateStatus::Failed);
            assert_eq!(report.exit_code, 0);
            assert_eq!(report.stdout, "executed");
            assert!(!report.stderr.contains("Lab gate did not execute"));
            assert!(report.stderr.starts_with("the gate modified tracked files"));
            assert_eq!(
                report.workspace_mutation.as_ref().unwrap().differing_paths,
                vec!["candidate.txt".to_string()]
            );
            let lab_receipt = report.lab_receipt.as_ref().unwrap();
            assert_eq!(lab_receipt["schema"], LAB_GATE_RECEIPT_SCHEMA);
            assert!(lab_receipt.get("executed").is_none());
            assert_eq!(report.step.status, PlanStepStatus::Failed);
            assert_eq!(
                HomeboyGateResult::from(report.clone()).status,
                HomeboyGateStatus::Failed
            );
        });
    }

    /// A runner that executes the real gate on its own clone, then lets the
    /// test break the wrapper the way production Lab infrastructure can.
    struct TamperingRunnerTransport {
        previous: Arc<dyn LabGateTransport>,
    }
    impl LabGateTransport for TamperingRunnerTransport {
        fn declared_runner(&self, command: &str) -> Result<Option<String>> {
            self.previous.declared_runner(command)
        }
        fn execute(
            &self,
            cwd: &Path,
            request: &LabGateRequest,
            supervision: Option<&GateSupervision>,
        ) -> Result<LabGateReceipt> {
            let Some(mode) = request.runner_id.strip_prefix("tamper-") else {
                return self.previous.execute(cwd, request, supervision);
            };
            match mode {
                "admission" => {
                    return Err(Error::new(
                        homeboy_core::ErrorCode::RunnerLabTransportFailure,
                        "Lab gate runner admission is stale or unavailable",
                        serde_json::json!({"gate_disposition":"unavailable", "stage":"admission"}),
                    ))
                }
                "terminal-wait" => {
                    return Err(Error::internal_unexpected("daemon job observation ended"))
                }
                _ => {}
            }
            let runner = tempfile::tempdir().unwrap();
            assert!(Command::new("git")
                .args(["clone", "-q"])
                .arg(cwd)
                .arg(runner.path())
                .status()
                .unwrap()
                .success());
            let mut receipt = execute_with_context(
                request.clone(),
                runner.path(),
                &runner_context(&request.runner_id),
            )?;
            match mode {
                // gate-execute exited 1 with a failed report its command never produced.
                "causeless-exit" => receipt.report.status = AgentTaskGateStatus::Failed,
                "unbound-receipt" => {
                    receipt.candidate = AgentTaskPromotionCandidate::NonGit {
                        disposition: "foreign".to_string(),
                    }
                }
                _ => {}
            }
            Ok(receipt)
        }
    }

    fn tampered_dispatch(script: &str, mode: &str) -> (LabGateRequest, AgentTaskGateReport) {
        let previous = active_transport();
        register_lab_gate_transport(Arc::new(TamperingRunnerTransport { previous }));
        let temp = tempfile::tempdir().unwrap();
        repository(temp.path());
        let request = mutating_gate(temp.path(), &format!("tamper-{mode}"), script);
        let report = dispatch(temp.path(), &request, None).unwrap();
        (request, report)
    }

    /// #17649: a gate is `failed` only when its declared command ran and
    /// returned non-zero, and that verdict carries command, exit and tail.
    #[test]
    fn only_a_command_that_ran_and_exited_non_zero_is_a_failed_gate() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let (request, report) = tampered_dispatch(
                "printf 'ran\\n'; printf 'assertion failed\\n' >&2; exit 3",
                "ran",
            );

            assert_eq!(report.status, AgentTaskGateStatus::Failed);
            assert_eq!(report.exit_code, 3);
            let evidence = report.failure_evidence.as_ref().unwrap();
            assert_eq!(evidence.exit_code, 3);
            assert_eq!(evidence.command, request.label);
            assert!(report.command.join(" ").contains("exit 3"));
            assert!(evidence.stderr_tail.contains("assertion failed"));
            assert_eq!(
                report.lab_receipt.as_ref().unwrap()["schema"],
                LAB_GATE_RECEIPT_SCHEMA
            );
        });
    }

    /// #17649: admission, terminal-wait, unbound receipts and a failed report
    /// no command produced are retryable `unavailable`, naming stage and cause.
    #[test]
    fn lab_infrastructure_failures_are_unavailable_with_stage_and_cause() {
        homeboy_core::test_support::with_isolated_home(|_| {
            for (mode, stage, code) in [
                ("admission", "admission", "runner.lab_transport_failure"),
                ("terminal-wait", "transport", "internal.unexpected"),
                (
                    "unbound-receipt",
                    "receipt_verification",
                    "validation.invalid_argument",
                ),
                ("causeless-exit", "gate_execute", "internal.unexpected"),
            ] {
                let (_, report) = tampered_dispatch("printf 'gate passed\\n' >&2", mode);

                assert_eq!(report.status, AgentTaskGateStatus::Unavailable, "{mode}");
                assert!(report.failure_evidence.is_none(), "{mode}");
                let receipt = report.lab_receipt.as_ref().unwrap();
                assert_eq!(receipt["executed"], false, "{mode}");
                assert_eq!(receipt["stage"], stage, "{mode}");
                assert_eq!(receipt["error_code"], code, "{mode}");
                assert_eq!(receipt["retryable"], true, "{mode}");
                assert_eq!(
                    HomeboyGateResult::from(report.clone()).status,
                    HomeboyGateStatus::Blocked,
                    "{mode}"
                );
                if mode == "causeless-exit" {
                    assert!(receipt["diagnostic"]["details"]["stderr_tail"]
                        .as_str()
                        .unwrap()
                        .contains("gate passed"));
                }
            }
        });
    }

    /// #17649: every non-passing receipt gives gate-execute a typed cause for
    /// its non-zero exit: the failed command, the non-execution stage, or
    /// `internal` with the stderr tail.
    #[test]
    fn gate_execute_never_exits_non_zero_without_a_typed_cause() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            repository(temp.path());
            let passed =
                execute_with_context(request(temp.path()), temp.path(), &lab_context()).unwrap();
            assert_eq!(gate_execute_failure(&passed), None);

            let failing = mutating_gate(temp.path(), "lab", "printf 'boom\\n' >&2; exit 4");
            let failed = execute_with_context(failing, temp.path(), &lab_context()).unwrap();
            let failure = gate_execute_failure(&failed).unwrap();
            assert_eq!(failure["code"], "gate.failed");
            assert_eq!(failure["details"]["exit_code"], 4);
            assert!(failure["details"]["command"]
                .as_str()
                .unwrap()
                .contains("exit 4"));
            assert!(failure["stderr_tail"].as_str().unwrap().contains("boom"));

            let mut causeless = failed.clone();
            causeless.report.exit_code = 0;
            let failure = gate_execute_failure(&causeless).unwrap();
            assert_eq!(failure["code"], "internal.unexpected");
            assert_eq!(failure["details"]["stage"], "gate_execute");
            assert!(failure["stderr_tail"].as_str().unwrap().contains("boom"));

            let setup = tempfile::tempdir().unwrap();
            repository_with_dependency_install(setup.path(), "echo install broke >&2; exit 1");
            let unavailable =
                execute_with_context(request(setup.path()), setup.path(), &lab_context()).unwrap();
            assert_eq!(unavailable.report.status, AgentTaskGateStatus::Unavailable);
            let failure = gate_execute_failure(&unavailable).unwrap();
            assert_eq!(failure["code"], "dependency_step_failed");
            assert_eq!(failure["details"]["stage"], "dependency_setup");
            assert!(!failure["message"].as_str().unwrap().is_empty());
        });
    }
}
