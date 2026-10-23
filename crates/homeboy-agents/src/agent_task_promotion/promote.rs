use homeboy_engine_primitives::content_hash;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
};

const CANONICAL_PATCH_CANDIDATE_LIMIT: usize = 16;
const CANONICAL_PATCH_BYTES_TOTAL_LIMIT: u64 = 1024 * 1024;
const DEFAULT_DECLARED_BASE_GIT_TIMEOUT: Duration = Duration::from_secs(30);
const DECLARED_BASE_GIT_TIMEOUT_ENV: &str = "HOMEBOY_GIT_BASE_PREFLIGHT_TIMEOUT_SECS";
const DECLARED_BASE_GIT_HEARTBEAT: Duration = Duration::from_secs(1);
const PROXY_ENV_KEYS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];
use crate::agent_task_gate::{
    AgentTaskGateCandidateCheckout, AgentTaskGateRevealPolicy, AgentTaskGateStatus,
    AgentTaskGateVisibility,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AGENT_TASK_AGGREGATE_SCHEMA};
use crate::agent_task_timeout_artifacts::{
    is_actionable_patch_artifact, is_empty_patch_artifact, is_patch_artifact_kind,
};
use homeboy_core::gate::HomeboyGateResult;
use homeboy_core::git::GitPushTransportClass;
use homeboy_core::{Error, Result};

use super::apply::verify_with_runtime_tmpdir;
use super::committed_changes::{adopted_candidate, git_lines, is_ancestor};
pub(crate) use super::patch::{normalize_promotion_patch, validate_artifact_content};
#[cfg(test)]
use super::tests::FakePromotionWorkspaceProvider;
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport, AgentTaskPromotionNotification,
    AgentTaskPromotionReport, AgentTaskPromotionRequest, AgentTaskPromotionSource,
    AgentTaskPromotionStatus, AgentTaskPromotionTarget, AgentTaskPromotionVerifiedBase,
    AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};

mod gate_run;

use gate_run::PromotionGateRun;

thread_local! {
    static GATE_SUPERVISION: RefCell<Option<Arc<crate::agent_task_gate::GateSupervision>>> = const { RefCell::new(None) };
    static PROMOTION_PROGRESS: RefCell<Option<PromotionProgressCallback>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static TEST_PROMOTION_OPERATIONS: RefCell<Option<FakePromotionWorkspaceProvider>> = const { RefCell::new(None) };
}

pub type PromotionProgressCallback = Arc<dyn Fn(&PromotionProgress) -> Result<()> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct PromotionProgress {
    pub phase: &'static str,
    pub gate: Option<String>,
    pub last_progress: Option<String>,
    pub output_tail: Option<String>,
}

pub fn with_promotion_progress<T>(
    progress: PromotionProgressCallback,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    PROMOTION_PROGRESS.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "promotion progress scopes cannot nest"
        );
        *slot.borrow_mut() = Some(progress);
        let result = operation();
        *slot.borrow_mut() = None;
        result
    })
}

pub(crate) fn emit_promotion_progress(
    phase: &'static str,
    gate: Option<String>,
    last_progress: Option<String>,
) {
    emit_promotion_progress_with_output(phase, gate, last_progress, None);
}

pub(crate) fn emit_promotion_progress_with_output(
    phase: &'static str,
    gate: Option<String>,
    last_progress: Option<String>,
    output_tail: Option<String>,
) {
    PROMOTION_PROGRESS.with(|slot| {
        if let Some(progress) = slot.borrow().as_ref() {
            let _ = progress(&PromotionProgress {
                phase,
                gate,
                last_progress,
                output_tail,
            });
        }
    });
}

pub(crate) fn promotion_cancellation() -> Arc<dyn Fn() -> bool + Send + Sync> {
    GATE_SUPERVISION.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|supervision| Arc::clone(&supervision.is_cancelled))
            .unwrap_or_else(|| Arc::new(|| false))
    })
}

/// The gate supervision scope the innermost `with_gate_supervision` opened on
/// this thread, if any. The shared gate runner consults it when a call site
/// did not thread explicit supervision, so a Lab gate request never silently
/// replaces the operator's declared budgets with built-in defaults (#15856).
pub(crate) fn active_gate_supervision() -> Option<Arc<crate::agent_task_gate::GateSupervision>> {
    GATE_SUPERVISION.with(|slot| slot.borrow().clone())
}

pub(crate) fn with_gate_supervision<T>(
    supervision: crate::agent_task_gate::GateSupervision,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    GATE_SUPERVISION.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "promotion gate supervision scopes cannot nest"
        );
        *slot.borrow_mut() = Some(Arc::new(supervision));
        let result = operation();
        *slot.borrow_mut() = None;
        result
    })
}

pub fn promote(options: AgentTaskPromotionRequest) -> Result<AgentTaskPromotionReport> {
    promote_with_checkpoint(options, |_| Ok(()))
}

/// Promote a patch while recording the recoverable post-apply boundary before
/// dependency materialization or verification is attempted.
pub fn promote_with_checkpoint(
    options: AgentTaskPromotionRequest,
    mut checkpoint: impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    // One promotion is one unit of work, so the store it records against
    // resolves once here and is passed down. The interior used to receive
    // `None` and re-resolve the environment partway through (#7505).
    let observation_store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let handback = lab_candidate_handback(&options);
    let mut report = promote_with_provider_and_checkpoint_internal(
        options,
        &mut checkpoint,
        &observation_store,
    )?;
    stamp_lab_offload(&mut report, handback);
    Ok(report)
}

pub(crate) fn promote_with_checkpoint_in_observation_store(
    options: AgentTaskPromotionRequest,
    observation_store: &homeboy_core::observation::ObservationStore,
    mut checkpoint: impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    let handback = lab_candidate_handback(&options);
    let mut report = if let Some(run_id) = options.source_run_id.clone() {
        observation_store.time_run_phase_with_outcome(
            &run_id,
            "promote",
            || {
                promote_with_provider_and_checkpoint_internal(
                    options,
                    &mut checkpoint,
                    observation_store,
                )
            },
            |report| report.status.gate_failed(),
        )?
    } else {
        promote_with_provider_and_checkpoint_internal(options, &mut checkpoint, observation_store)?
    };
    stamp_lab_offload(&mut report, handback);
    Ok(report)
}

fn stamp_lab_offload(report: &mut AgentTaskPromotionReport, candidate_handback: Value) {
    if let Some(runner_id) = crate::agent_task_lifecycle::execution_runner_id() {
        report.provenance["lab_offload"] = json!({
            "runner_id": runner_id,
            "source_aggregate": report.source.path,
            "source_artifact": report.patch_artifact.path,
            "target_worktree": report.to_worktree,
            "target_workspace": report.target.path,
            "candidate_handback": candidate_handback,
        });
    }
}

/// The harvested candidate commit and the runner artifact bundling it: the
/// controller fetches the commit itself, never a patch (#16303).
fn lab_candidate_handback(options: &AgentTaskPromotionRequest) -> Value {
    let source = serde_json::from_str(&options.source).unwrap_or(Value::Null);
    let Ok((_, outcome)) = select_outcome(source, options.task_id.as_deref()) else {
        return Value::Null;
    };
    let kind = crate::agent_task::CANDIDATE_BUNDLE_ARTIFACT_KIND;
    match (
        outcome.artifacts.iter().find(|a| a.kind == kind),
        &outcome.candidate,
    ) {
        (Some(bundle), Some(candidate)) => json!({
            "run_id": bundle.metadata["run_id"],
            "task_id": outcome.task_id,
            "artifact_id": bundle.id,
            "candidate": candidate,
        }),
        _ => Value::Null,
    }
}

/// Rebuild a full proof for a clean candidate from a durable post-apply report.
/// The reverse apply check proves the original artifact remains in the candidate
/// before any gate result is trusted.
pub fn resume_promoted_patch(
    options: AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
) -> Result<AgentTaskPromotionReport> {
    // One promotion is one unit of work, so the store it records against
    // resolves once here and is passed down. The interior used to receive
    // `None` and re-resolve the environment partway through (#7505).
    let observation_store = homeboy_core::observation::ObservationStore::open_initialized()?;
    resume_promoted_patch_internal(
        options,
        target_path,
        previous,
        &observation_store,
        false,
        None,
        None,
    )
}

pub(crate) fn resume_promoted_patch_in_observation_store(
    options: AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionReport> {
    resume_promoted_patch_internal(
        options,
        target_path,
        previous,
        observation_store,
        false,
        None,
        None,
    )
}

/// Re-run corrected gates against an already-applied candidate while preserving
/// all candidate, base, target, source, and artifact resume validation.
pub(crate) fn resume_promoted_patch_replacement_gates_in_observation_store<'a>(
    options: AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
    gate_workspace: Option<&Path>,
    observation_store: &homeboy_core::observation::ObservationStore,
    before_gates: impl FnOnce() -> Result<()> + 'a,
) -> Result<AgentTaskPromotionReport> {
    resume_promoted_patch_internal(
        options,
        target_path,
        previous,
        observation_store,
        true,
        gate_workspace,
        Some(Box::new(before_gates)),
    )
}

fn resume_promoted_patch_internal<'a>(
    options: AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
    observation_store: &homeboy_core::observation::ObservationStore,
    replacement_gates: bool,
    gate_workspace: Option<&Path>,
    before_gates: Option<Box<dyn FnOnce() -> Result<()> + 'a>>,
) -> Result<AgentTaskPromotionReport> {
    let ResumePromotedPatchAdmission {
        source_kind,
        outcome,
        artifact,
        patch_path,
        normalized_patch,
        command_evidence,
    } = resume_promoted_patch_admission(
        &options,
        target_path,
        previous,
        observation_store,
        replacement_gates,
    )?;
    // A resumed checkpoint verifies the already-applied candidate. Its base is
    // the immutable observation made before apply, not a new moving origin read.
    let verified_base = previous
        .get("verified_base")
        .filter(|base| !base.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| {
            Error::validation_invalid_argument(
                "promotion",
                "promotion resume durable resolved base is invalid",
                None,
                None,
            )
        })?;
    let verified_base = match verified_base {
        Some(verified_base) => Some(verified_base),
        None => capture_gate_verified_base(target_path, options.base_ref.as_deref())?,
    };
    let expected_candidate = previous
        .pointer("/provenance/candidate")
        .filter(|candidate| !candidate.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| {
            Error::validation_invalid_argument(
                "promotion.provenance.candidate",
                "promotion resume durable candidate fingerprint is invalid",
                None,
                None,
            )
        })?;
    if let Some(before_gates) = before_gates {
        before_gates()?;
    }
    let gates = run_promotion_gates(
        &options,
        target_path,
        expected_candidate.as_ref(),
        outcome.candidate.as_ref(),
        gate_workspace,
        observation_store,
    )?;
    let target =
        AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), Some(target_path));
    let candidate = if gates.status.patch_promoted() {
        Some(crate::agent_task_promotion::candidate_fingerprint(
            &target_path.display().to_string(),
        )?)
    } else {
        None
    };
    let operator_notification =
        promotion_notification_with_gate_summary(gates.status, &target, &gates.deterministic_gates);
    let report = AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: gates.status,
        source: promotion_source(&source_kind, &outcome, &options),
        to_worktree: options.to_worktree,
        target: target.clone(),
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: artifact.id,
            kind: artifact.kind,
            path: patch_path.display().to_string(),
            sha256: artifact.sha256,
        },
        changed_files: persisted_changed_files(normalized_patch.changed_files, candidate.as_ref()),
        command_evidence,
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
        repository_integrity_evidence: options.repository_integrity_evidence.clone(),
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.metadata,
            "worktree_path": target_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate_checkout_setup": gates.candidate_setup,
            "destination_gate_setup": gates.destination_gate_setup,
            "candidate_checkout": gates.candidate_checkout,
            "candidate": candidate,
            "resumed_post_apply_promotion": true,
            "resume_contract": previous.pointer("/provenance/resume_contract"),
        }),
        operator_notification,
    };
    Ok(report)
}

struct ResumePromotedPatchAdmission {
    source_kind: String,
    outcome: AgentTaskOutcome,
    artifact: AgentTaskArtifact,
    patch_path: PathBuf,
    normalized_patch: super::patch::NormalizedPromotionPatch,
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
}

struct PatchArtifactAdmission {
    path: PathBuf,
    normalized_patch: super::patch::NormalizedPromotionPatch,
}

fn patch_artifact_admission(
    artifact: &AgentTaskArtifact,
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionRequest,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<PatchArtifactAdmission> {
    let path = resolve_artifact_path(
        artifact,
        &outcome.task_id,
        options.source_run_id.as_deref(),
        options.source_path.as_deref(),
        observation_store,
    )?;
    let patch = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read patch artifact {}", path.display())),
        )
    })?;
    validate_artifact_content(artifact, &patch)?;
    if patch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "patch promotion requires a non-empty patch",
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(PatchArtifactAdmission {
        path,
        normalized_patch: normalize_promotion_patch(&patch, &options.to_worktree)?,
    })
}

fn resume_promoted_patch_admission(
    options: &AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
    observation_store: &homeboy_core::observation::ObservationStore,
    replacement_gates: bool,
) -> Result<ResumePromotedPatchAdmission> {
    validate_resume_provenance(options, target_path, previous)?;
    let source_value: Value = serde_json::from_str(&options.source).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion source".to_string()),
            Some(options.source.clone()),
        )
    })?;
    let (source_kind, outcome) = select_outcome(source_value, options.task_id.as_deref())?;
    let artifact = select_patch_artifact(&outcome, options.artifact_id.as_deref())?;
    let PatchArtifactAdmission {
        path: patch_path,
        normalized_patch,
    } = patch_artifact_admission(&artifact, &outcome, &options, observation_store)?;
    validate_resume_candidate(
        options,
        target_path,
        previous,
        &outcome,
        &artifact,
        replacement_gates,
    )?;
    let command_evidence = vec![verify_patch_is_present(
        target_path,
        &normalized_patch.content,
    )?];
    Ok(ResumePromotedPatchAdmission {
        source_kind,
        outcome,
        artifact,
        patch_path,
        normalized_patch,
        command_evidence,
    })
}

fn validate_resume_provenance(
    options: &AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
) -> Result<()> {
    let previous_status = previous.get("status").and_then(Value::as_str);
    if !(matches!(
        previous_status,
        Some("gate_failed" | "verification_pending")
    ) || options.gates.rerun_completed_gates && previous_status == Some("applied"))
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume requires a durable post-apply promotion or an explicit completed-gate rerun for an applied promotion",
            None,
            None,
        ));
    }
    let previous_run = previous
        .get("source_run_id")
        .and_then(Value::as_str)
        .or_else(|| previous.pointer("/source/run_id").and_then(Value::as_str));
    if previous_run != options.source_run_id.as_deref() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume source run does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    if previous.get("to_worktree").and_then(Value::as_str) != Some(options.to_worktree.as_str())
        || previous.pointer("/target/worktree").and_then(Value::as_str)
            != Some(options.to_worktree.as_str())
        || previous.pointer("/target/path").and_then(Value::as_str)
            != Some(target_path.to_string_lossy().as_ref())
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume target does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    Ok(())
}

fn validate_resume_candidate(
    options: &AgentTaskPromotionRequest,
    target_path: &Path,
    previous: &Value,
    outcome: &AgentTaskOutcome,
    artifact: &AgentTaskArtifact,
    replacement_gates: bool,
) -> Result<()> {
    if previous.pointer("/source/task_id").and_then(Value::as_str) != Some(&outcome.task_id)
        || previous
            .pointer("/patch_artifact/id")
            .and_then(Value::as_str)
            != Some(&artifact.id)
        || previous
            .pointer("/patch_artifact/kind")
            .and_then(Value::as_str)
            != Some(&artifact.kind)
        || previous
            .pointer("/patch_artifact/sha256")
            .and_then(Value::as_str)
            != artifact.sha256.as_deref()
    {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume source task or patch artifact does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    let expected_inputs = json!({
        "base_ref": options.base_ref,
        "task_base_sha": options.task_base_sha,
        "candidate_ref": options.candidate_ref,
    });
    let recorded_inputs = previous
        .pointer("/provenance/resume_contract/inputs")
        .or_else(|| previous.pointer("/provenance/resume_inputs"));
    if recorded_inputs.is_some_and(|recorded| {
        recorded != &expected_inputs
            && !legacy_base_correction_matches(previous, recorded, &expected_inputs)
    }) {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume base or candidate input does not match the durable post-apply promotion",
            None,
            None,
        ));
    }
    if !replacement_gates {
        if let Some(recorded_gates) = previous.pointer("/provenance/resume_contract/gates") {
            let actual_gates = serde_json::to_value(&options.gates)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            if recorded_gates != &actual_gates {
                return Err(Error::validation_invalid_argument(
                    "promotion",
                    "promotion resume deterministic gate contract does not match the durable post-apply promotion",
                    None,
                    None,
                ));
            }
        }
    }
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(target_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !status.status.success() {
        return Err(Error::validation_invalid_argument(
            "path",
            "promotion resume target is not an accessible Git worktree",
            Some(target_path.display().to_string()),
            None,
        ));
    }
    let expected = previous
        .pointer("/provenance/candidate")
        .filter(|candidate| !candidate.is_null())
        .cloned();
    if !status.stdout.is_empty() && expected.is_none() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume found a dirty target without an exact post-apply candidate fingerprint; rerun from a clean target",
            None,
            None,
        ));
    }
    if let Some(expected) = expected {
        let expected = serde_json::from_value(expected).map_err(|_| {
            Error::validation_invalid_argument(
                "promotion",
                "promotion resume durable candidate fingerprint is invalid",
                None,
                None,
            )
        })?;
        let actual = crate::agent_task_promotion::candidate_fingerprint(
            target_path.to_string_lossy().as_ref(),
        )?;
        if actual != expected {
            let mut error = Error::validation_invalid_argument(
                "promotion",
                "promotion resume target differs from the exact checkpointed applied candidate",
                Some(target_path.display().to_string()),
                None,
            );
            // Continuing would authenticate a different candidate. Tell the
            // Cook recovery projection to preserve this checkpoint and fork.
            error.details["recovery"] = serde_json::json!({
                "action": "fork_replacement",
            });
            return Err(error);
        }
    }
    if let Some(recorded_base) = previous.get("verified_base") {
        if recorded_base.is_null()
            || recorded_base.get("base").and_then(Value::as_str) != options.base_ref.as_deref()
        {
            return Err(Error::validation_invalid_argument(
                "base_ref",
                "promotion resume declared base does not match the durable post-apply promotion",
                None,
                None,
            ));
        }
    }
    Ok(())
}

/// #9400 checkpoints persisted after applying a patch but before the declared
/// base was validated. Permit exactly that historical base correction only;
/// artifact, target, and candidate authentication remain mandatory below.
fn legacy_base_correction_matches(previous: &Value, recorded: &Value, requested: &Value) -> bool {
    if previous.get("status").and_then(Value::as_str) != Some("verification_pending")
        || previous.pointer("/provenance/post_apply") != Some(&Value::Bool(true))
        || previous
            .get("verified_base")
            .is_some_and(|base| !base.is_null())
    {
        return false;
    }
    let mut recorded = recorded.clone();
    let Some(recorded_inputs) = recorded.as_object_mut() else {
        return false;
    };
    recorded_inputs.insert(
        "base_ref".to_string(),
        requested.get("base_ref").cloned().unwrap_or(Value::Null),
    );
    &recorded == requested
}

fn verify_patch_is_present(
    target_path: &Path,
    patch: &str,
) -> Result<AgentTaskPromotionCommandReport> {
    use std::io::Write;

    let mut child = std::process::Command::new("git")
        .args(["apply", "--reverse", "--check", "-"])
        .current_dir(target_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(patch.as_bytes())
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let report = AgentTaskPromotionCommandReport {
        command: vec![
            "git".to_string(),
            "apply".to_string(),
            "--reverse".to_string(),
            "--check".to_string(),
            "-".to_string(),
        ],
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        capture: Default::default(),
    };
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion resume could not prove the recorded patch is present in the clean target",
            None,
            None,
        ));
    }
    Ok(report)
}

#[cfg(test)]
// Provider-injection seam: production promotes through `promote`.
pub(crate) fn promote_with_provider(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
) -> Result<AgentTaskPromotionReport> {
    promote_with_provider_and_checkpoint(options, provider, &mut |_| Ok(()))
}

#[cfg(test)]
// Checkpoint seam reached only by the promotion test shards.
pub(super) fn promote_with_provider_and_checkpoint(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
) -> Result<AgentTaskPromotionReport> {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let observation_store = homeboy_core::observation::ObservationStore::open_initialized_in_roots(
        &context.path_roots(),
    )?;
    with_test_promotion_operations(provider, || {
        promote_with_provider_and_checkpoint_internal(options, checkpoint, &observation_store)
    })
}

#[cfg(test)]
pub(super) fn promote_with_provider_in_observation_store(
    options: AgentTaskPromotionRequest,
    provider: &mut FakePromotionWorkspaceProvider,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionReport> {
    with_test_promotion_operations(provider, || {
        promote_with_provider_and_checkpoint_internal(options, &mut |_| Ok(()), observation_store)
    })
}

#[cfg(test)]
fn with_test_promotion_operations<T>(
    provider: &mut FakePromotionWorkspaceProvider,
    operation: impl FnOnce() -> T,
) -> T {
    TEST_PROMOTION_OPERATIONS.with(|slot| {
        assert!(slot.borrow().is_none(), "promotion test scopes cannot nest");
        *slot.borrow_mut() = Some(std::mem::take(provider));
        let result = operation();
        *provider = slot.borrow_mut().take().expect("promotion test provider");
        result
    })
}

fn materialized_promotion_workspace() -> Result<Option<PathBuf>> {
    materialized_promotion_workspace_in_context(
        crate::agent_task_lifecycle::execution_runner_id().as_deref(),
    )
}

fn materialized_promotion_workspace_in_context(runner_id: Option<&str>) -> Result<Option<PathBuf>> {
    if runner_id.is_none_or(|runner_id| runner_id.trim().is_empty()) {
        return Ok(None);
    }
    std::fs::canonicalize(std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve promotion execution directory".to_string()),
        )
    })?)
    .map(Some)
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve promotion execution directory".to_string()),
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "gate execution requires explicit runtime isolation inputs"
)]
fn verify_promotion_gate(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    gate_environment: &crate::agent_task_gate::AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[crate::agent_task_gate::AgentTaskGatePackageArtifactRequirement],
) -> Result<crate::agent_task_gate::AgentTaskGateReport> {
    #[cfg(test)]
    if TEST_PROMOTION_OPERATIONS.with(|slot| slot.borrow().is_some()) {
        return TEST_PROMOTION_OPERATIONS.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("promotion test provider")
                .verify_with_runtime_tmpdir(
                    cwd,
                    index,
                    command,
                    visibility,
                    reveal_policy,
                    runtime_tmpdir,
                    gate_environment,
                    package_artifacts,
                )
        });
    }
    verify_with_runtime_tmpdir(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        gate_environment,
        package_artifacts,
    )
}

fn promote_with_provider_and_checkpoint_internal(
    options: AgentTaskPromotionRequest,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<AgentTaskPromotionReport> {
    emit_promotion_progress(
        "apply",
        None,
        Some("validating promotion input".to_string()),
    );
    validate_workspace_handle(&options.to_worktree)?;
    let source_value: Value = serde_json::from_str(&options.source).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task promotion source".to_string()),
            Some(options.source.clone()),
        )
    })?;
    let (source_kind, outcome) = select_outcome(source_value, options.task_id.as_deref())?;

    let failed_candidate_adoption = options.candidate_ref.is_some()
        && outcome.status == AgentTaskOutcomeStatus::Failed
        && has_candidate_adoption_recovery_eligibility(&outcome);
    if !matches!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded
            | AgentTaskOutcomeStatus::CandidateRecoverable
            | AgentTaskOutcomeStatus::NoOp
    ) && !failed_candidate_adoption
    {
        let problem = if options.candidate_ref.is_some()
            && outcome.status == AgentTaskOutcomeStatus::Failed
        {
            "immutable candidate adoption requires explicit durable pre-provider recovery eligibility; legacy or provider/test failures remain ineligible. Retry the cook or record a new eligible pre-provider failure through Homeboy."
                .to_string()
        } else {
            format!(
                "promotion requires a succeeded, recoverable-candidate, or no-op outcome; task {} has status {:?}",
                outcome.task_id, outcome.status
            )
        };
        return Err(Error::validation_invalid_argument(
            "source", problem, None, None,
        ));
    }

    // Adoption names the candidate commit directly; it bypasses provider
    // artifacts but uses the ordinary candidate admission, gates and
    // durable checkpoint (#16303).
    if options.candidate_ref.is_some() {
        let adopted = adopted_candidate(&options)?;
        return promote_candidate_commit(
            &options,
            checkpoint,
            observation_store,
            &source_kind,
            &outcome,
            &adopted.candidate,
            Some(adopted.provenance),
        );
    }

    if let Some(candidate) = outcome.candidate.as_ref() {
        return promote_candidate_commit(
            &options,
            checkpoint,
            observation_store,
            &source_kind,
            &outcome,
            candidate,
            None,
        );
    }
    if let Some(error) = patch_only_outcome_error(&outcome) {
        return Err(error);
    }
    if outcome.status == AgentTaskOutcomeStatus::NoOp {
        return Err(Error::validation_invalid_argument(
            "source",
            "no-op promotion requires an audited committed candidate after the task base",
            None,
            None,
        ));
    }
    if outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable {
        let mut error = Error::validation_invalid_argument(
            "source",
            "recoverable-candidate promotion requires a harvested candidate commit (#16303)",
            Some(outcome.task_id.clone()),
            None,
        );
        error.details["recoverable"] = json!(false);
        return Err(error);
    }
    promote_no_change(&options, observation_store, &source_kind, &outcome)
}

/// Promote the harvested candidate commit (#16303). The commit is the only
/// candidate identity: it is verified against its recorded tree and base, the
/// destination must still hold that base, and the destination's working tree
/// is materialized from the commit while its branch stays checked out. The
/// destination is then checkpointed, gated, and the verdict reported.
fn promote_candidate_commit(
    options: &AgentTaskPromotionRequest,
    checkpoint: &mut impl FnMut(&AgentTaskPromotionReport) -> Result<()>,
    observation_store: &homeboy_core::observation::ObservationStore,
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    candidate: &crate::agent_task::AgentTaskCandidate,
    adoption: Option<Value>,
) -> Result<AgentTaskPromotionReport> {
    let materialized_workspace = materialized_promotion_workspace()?;
    // An adopted revision may be promoted in the source worktree that holds it.
    let adopted_source = adoption
        .as_ref()
        .and(options.source_worktree_path.as_deref());
    let target_path = resolve_promotion_target_path(
        &options.to_worktree,
        materialized_workspace.as_deref(),
        adopted_source,
    )?
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            "candidate promotion requires a resolvable Git destination worktree",
            Some(options.to_worktree.clone()),
            None,
        )
    })?;
    let (changed_files, materialized) = admit_candidate_commit(&target_path, candidate)?;
    if adoption.is_none() && outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable {
        verify_recoverable_candidate_provenance(&target_path, options, outcome, candidate)?;
    }
    // The declared base is validated before the destination is touched (#9400).
    let verified_base = if options.dry_run {
        None
    } else {
        capture_gate_verified_base(&target_path, options.base_ref.as_deref())?
    };
    // Patch artifacts are derived evidence of this candidate, never its input.
    // A resumable checkpoint still names a verified harvested patch, which the
    // resume contract re-authenticates until resume reads the commit (#16303).
    let harvested_patch = (adoption.is_none())
        .then(|| select_patch_artifact(outcome, options.artifact_id.as_deref()).ok())
        .flatten()
        .filter(is_actionable_patch_artifact)
        .and_then(|artifact| {
            let admission =
                patch_artifact_admission(&artifact, outcome, options, observation_store).ok()?;
            Some(AgentTaskPromotionArtifactRef {
                id: artifact.id,
                kind: artifact.kind,
                path: admission.path.display().to_string(),
                sha256: artifact.sha256,
            })
        });
    // Without one, the candidate's identity is the digest of its own delta.
    let patch_artifact = match harvested_patch {
        Some(artifact) => artifact,
        None => AgentTaskPromotionArtifactRef {
            id: candidate.candidate_sha.clone(),
            kind: "candidate_commit".to_string(),
            path: String::new(),
            sha256: Some(content_hash::sha256_hex(
                git_output(
                    &target_path,
                    &[
                        "diff",
                        "--binary",
                        &candidate.base_sha,
                        &candidate.candidate_sha,
                    ],
                )?
                .as_bytes(),
            )),
        },
    };
    let worktree_path = (!options.dry_run).then_some(target_path);
    let target = AgentTaskPromotionTarget::from_worktree(
        options.to_worktree.clone(),
        worktree_path.as_deref(),
    );
    let post_apply = worktree_path
        .as_deref()
        .map(|worktree_path| {
            if !materialized {
                emit_promotion_progress("apply", None, Some("materializing candidate".to_string()));
                materialize_candidate_commit(worktree_path, candidate)?;
            }
            let report = post_apply_report(
                options,
                source_kind,
                outcome,
                patch_artifact.clone(),
                changed_files.clone(),
                Vec::new(),
                target.clone(),
                worktree_path,
                &outcome.schema,
                Value::Null,
                verified_base.clone(),
            )?;
            checkpoint(&report)?;
            Ok::<_, Error>(report)
        })
        .transpose()?;
    // Gates can create incidental files. Feedback must retain the identity
    // that was captured immediately after materializing the candidate.
    let destination = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("candidate").cloned())
        .and_then(|value| serde_json::from_value(value).ok());
    let gates = match worktree_path.as_deref() {
        Some(worktree_path) => run_promotion_gates(
            options,
            worktree_path,
            destination.as_ref(),
            Some(candidate),
            None,
            observation_store,
        )?,
        None => PromotionGateRun::without_gates(options.dry_run),
    };
    let operator_notification =
        promotion_notification_with_gate_summary(gates.status, &target, &gates.deterministic_gates);
    let gate_feedback_baseline = post_apply
        .as_ref()
        .and_then(|report| report.provenance.get("gate_feedback_baseline").cloned());
    let provenance = merge_provenance(
        json!({
            "source_schema": outcome.schema,
            "artifact_metadata": Value::Null,
            "worktree_path": worktree_path,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate_checkout_setup": gates.candidate_setup,
            "destination_gate_setup": gates.destination_gate_setup,
            "candidate_checkout": gates.candidate_checkout,
            "candidate": destination,
            "destination_baseline": destination,
            "gate_feedback_baseline": gate_feedback_baseline,
            "change_source": "candidate_commit",
            "candidate_commit": candidate,
        }),
        adoption,
    );
    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: gates.status,
        source: promotion_source(source_kind, outcome, options),
        to_worktree: options.to_worktree.clone(),
        target,
        patch_artifact,
        changed_files,
        command_evidence: Vec::new(),
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
        repository_integrity_evidence: options.repository_integrity_evidence.clone(),
        provenance,
        operator_notification,
    })
}

fn merge_provenance(mut base: Value, extra: Option<Value>) -> Value {
    if let (Some(base), Some(Value::Object(extra))) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// The commit trailers harvest records on a candidate commit: the producing
/// run and task are part of the immutable candidate itself.
pub(crate) const CANDIDATE_RUN_TRAILER: &str = "Homeboy-Run";
pub(crate) const CANDIDATE_TASK_TRAILER: &str = "Homeboy-Task";
pub(crate) const CANDIDATE_WORKSPACE_TRAILER: &str = "Homeboy-Workspace";

/// A recoverable candidate (e.g. left behind by a provider timeout) is only
/// admitted when it was harvested by the producing run and task this promotion
/// names, on the recorded task base (#16303). The commit's presence in the
/// destination repository was already established by admission.
fn verify_recoverable_candidate_provenance(
    destination: &Path,
    options: &AgentTaskPromotionRequest,
    outcome: &AgentTaskOutcome,
    candidate: &crate::agent_task::AgentTaskCandidate,
) -> Result<()> {
    let message = git_output(
        destination,
        &["show", "-s", "--format=%B", &candidate.candidate_sha],
    )?;
    let trailer = |key: &str| {
        message
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}: ")))
            .map(|value| value.trim().to_string())
    };
    let mismatch = |check: &str, expected: &str, actual: Option<String>| {
        Err(CandidateAdmissionError::ProvenanceMismatch {
            candidate_sha: candidate.candidate_sha.clone(),
            check: check.to_string(),
            expected: expected.to_string(),
            actual,
        }
        .into())
    };
    let task = trailer(CANDIDATE_TASK_TRAILER);
    if task.as_deref() != Some(outcome.task_id.as_str()) {
        return mismatch("task", &outcome.task_id, task);
    }
    if let Some(run_id) = options.source_run_id.as_deref() {
        let run = trailer(CANDIDATE_RUN_TRAILER);
        if run.as_deref() != Some(run_id) {
            return mismatch("run", run_id, run);
        }
    }
    if trailer(CANDIDATE_WORKSPACE_TRAILER).is_none_or(|workspace| workspace.is_empty()) {
        return mismatch("workspace", "the harvesting attempt workspace", None);
    }
    if let Some(task_base) = options.task_base_sha.as_deref() {
        // A Cook launched into a dirty destination harvests against a derived
        // baseline commit on top of the task base.
        if !is_ancestor(destination, task_base, &candidate.base_sha)? {
            return mismatch("base", task_base, Some(candidate.base_sha.clone()));
        }
    }
    Ok(())
}

/// Why a harvested candidate commit cannot be promoted (#16303).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CandidateAdmissionError {
    /// The commit is not present in the destination repository.
    MissingCommit { candidate_sha: String },
    /// The commit, or the destination materialized from it, does not have the
    /// tree harvest recorded, so gating one would not gate the other.
    TreeMismatch {
        candidate_sha: String,
        candidate_tree: String,
        actual_tree: String,
    },
    /// The recorded base is not an ancestor of the commit.
    BaseNotAncestor {
        base_sha: String,
        candidate_sha: String,
    },
    /// A recoverable candidate was not harvested by the producing run, task
    /// or base this promotion names.
    ProvenanceMismatch {
        candidate_sha: String,
        check: String,
        expected: String,
        actual: Option<String>,
    },
    /// The destination no longer holds the candidate's base.
    DestinationMoved {
        base_sha: String,
        base_tree: String,
        destination_head: String,
        destination_tree: String,
    },
}

impl std::fmt::Display for CandidateAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCommit { candidate_sha } => write!(
                formatter,
                "candidate commit {candidate_sha} is not present in the promotion destination repository"
            ),
            Self::TreeMismatch { candidate_sha, candidate_tree, actual_tree } => write!(
                formatter,
                "harvested candidate {candidate_sha} records tree {candidate_tree}, but promotion found tree {actual_tree}"
            ),
            Self::BaseNotAncestor { base_sha, candidate_sha } => write!(
                formatter,
                "candidate base {base_sha} is not an ancestor of candidate commit {candidate_sha}"
            ),
            Self::ProvenanceMismatch { candidate_sha, check, expected, actual } => write!(
                formatter,
                "recoverable candidate {candidate_sha} was harvested for {check} {}, not the producing {check} {expected}",
                actual.as_deref().unwrap_or("<unrecorded>")
            ),
            Self::DestinationMoved { base_sha, base_tree, destination_head, destination_tree } => write!(
                formatter,
                "promotion destination moved: it is at {destination_head} with tree {destination_tree}, not candidate base {base_sha} with tree {base_tree}"
            ),
        }
    }
}

impl std::error::Error for CandidateAdmissionError {}

impl From<CandidateAdmissionError> for Error {
    fn from(error: CandidateAdmissionError) -> Self {
        let mut details = serde_json::to_value(&error).unwrap_or_default();
        details["field"] = json!("promotion.candidate");
        Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            error.to_string(),
            details,
        )
        .with_source(error)
    }
}

/// Admit the candidate commit into `destination`. Returns its changed files
/// (`git diff --name-only base candidate`) and whether the destination already
/// holds the candidate.
fn admit_candidate_commit(
    destination: &Path,
    candidate: &crate::agent_task::AgentTaskCandidate,
) -> Result<(Vec<String>, bool)> {
    let crate::agent_task::AgentTaskCandidate {
        base_sha,
        candidate_sha,
        tree_sha,
    } = candidate;
    let commit_tree = git_output(
        destination,
        &[
            "rev-parse",
            "--verify",
            &format!("{candidate_sha}^{{commit}}^{{tree}}"),
        ],
    )
    .map_err(|_| CandidateAdmissionError::MissingCommit {
        candidate_sha: candidate_sha.clone(),
    })?;
    if &commit_tree != tree_sha {
        return Err(CandidateAdmissionError::TreeMismatch {
            candidate_sha: candidate_sha.clone(),
            candidate_tree: tree_sha.clone(),
            actual_tree: commit_tree,
        }
        .into());
    }
    if !is_ancestor(destination, base_sha, candidate_sha)? {
        return Err(CandidateAdmissionError::BaseNotAncestor {
            base_sha: base_sha.clone(),
            candidate_sha: candidate_sha.clone(),
        }
        .into());
    }
    // The destination holds the base when its working tree is the base tree
    // on a HEAD the base descends from (a gate-feedback base commits the prior
    // candidate on top of the destination HEAD). An in-place attempt leaves the
    // destination already holding the candidate on the candidate's history.
    let base_tree = git_output(destination, &["rev-parse", &format!("{base_sha}^{{tree}}")])?;
    let (destination_head, destination_tree) = destination_head_and_tree(destination)?;
    let materialized = &destination_tree == tree_sha
        && is_ancestor(destination, &destination_head, candidate_sha)?;
    if !materialized
        && (destination_tree != base_tree
            || !is_ancestor(destination, &destination_head, base_sha)?)
    {
        return Err(CandidateAdmissionError::DestinationMoved {
            base_sha: base_sha.clone(),
            base_tree,
            destination_head,
            destination_tree,
        }
        .into());
    }
    let changed_files = git_lines(
        destination,
        &["diff", "--name-only", base_sha, candidate_sha],
    )?;
    Ok((changed_files, materialized))
}

fn destination_head_and_tree(destination: &Path) -> Result<(String, String)> {
    match crate::agent_task_promotion::candidate_fingerprint(&destination.display().to_string())? {
        crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } => {
            Ok((fingerprint.head, fingerprint.tree))
        }
        crate::agent_task_promotion::AgentTaskPromotionCandidate::NonGit { disposition } => {
            Err(Error::validation_invalid_argument(
                "to_worktree",
                format!("candidate promotion requires a Git destination ({disposition})"),
                Some(destination.display().to_string()),
                None,
            ))
        }
    }
}

/// Switch the destination's working tree from the candidate base to the
/// candidate commit. HEAD and the destination's own index stay untouched, so
/// the destination branch remains checked out with the candidate as its
/// uncommitted change.
fn materialize_candidate_commit(
    destination: &Path,
    candidate: &crate::agent_task::AgentTaskCandidate,
) -> Result<()> {
    // A temporary index seeded with the base lets no-overlay restore also
    // delete base files the destination's own index does not track.
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(error.to_string(), Some("create candidate index".into()))
    })?;
    let env = [("GIT_INDEX_FILE".into(), index.path().display().to_string())];
    let source = format!("--source={}", candidate.candidate_sha);
    for args in [
        &["read-tree", candidate.base_sha.as_str()][..],
        &["restore", &source, "--worktree", "--", ":/"],
    ] {
        homeboy_core::git::run_git_bytes_with_env(
            destination,
            args,
            "materialize candidate",
            &env,
        )?;
    }
    let (_, destination_tree) = destination_head_and_tree(destination)?;
    if destination_tree != candidate.tree_sha {
        return Err(CandidateAdmissionError::TreeMismatch {
            candidate_sha: candidate.candidate_sha.clone(),
            candidate_tree: candidate.tree_sha.clone(),
            actual_tree: destination_tree,
        }
        .into());
    }
    Ok(())
}

/// An attempt that produced no candidate commit and no patch changed nothing.
fn promote_no_change(
    options: &AgentTaskPromotionRequest,
    observation_store: &homeboy_core::observation::ObservationStore,
    source_kind: &str,
    outcome: &AgentTaskOutcome,
) -> Result<AgentTaskPromotionReport> {
    let artifact = outcome
        .artifacts
        .iter()
        .find(|a| is_empty_patch_artifact(a));
    let worktree_path = options.source_worktree_path.as_deref();
    let target =
        AgentTaskPromotionTarget::from_worktree(options.to_worktree.clone(), worktree_path);
    // Homeboy proved this attempt changed nothing. There is no candidate:
    // gating the unchanged tree would only report the destination's prior
    // state as this attempt's verdict, and an empty candidate must never
    // read as green (#15294).
    let unchanged_attempt = outcome.is_verified_unchanged_attempt();
    let gates = match worktree_path {
        Some(worktree_path) if !unchanged_attempt => {
            run_promotion_gates(options, worktree_path, None, None, None, observation_store)?
        }
        _ => PromotionGateRun::without_gates(options.dry_run),
    };
    let status = match gates.status {
        _ if unchanged_attempt => AgentTaskPromotionStatus::NoChanges,
        AgentTaskPromotionStatus::Applied => AgentTaskPromotionStatus::VerifiedNoChanges,
        AgentTaskPromotionStatus::GateFailed => AgentTaskPromotionStatus::NoChangesGateFailed,
        _ => AgentTaskPromotionStatus::NoChanges,
    };
    let operator_notification =
        promotion_notification_with_gate_summary(status, &target, &gates.deterministic_gates);
    let verified_revision = target.head.clone();
    let candidate = target
        .path
        .as_deref()
        .map(crate::agent_task_promotion::candidate_fingerprint)
        .transpose()?;
    // An empty provider patch can be a review of a candidate that was
    // already committed on the declared destination. Retain that immutable
    // candidate delta so Cook can distinguish it from a base-equal no-op.
    let verified_base = worktree_path
        .zip(options.base_ref.as_deref())
        .map(|(path, base)| capture_gate_verified_base(path, Some(base)))
        .transpose()?
        .flatten();
    // Commits already on the destination are not this attempt's candidate
    // when Homeboy proved the attempt itself changed nothing.
    let changed_files = verified_base
        .as_ref()
        .filter(|_| !unchanged_attempt)
        .map(|base| {
            git_lines(
                worktree_path.expect("verified base requires a worktree path"),
                &["diff", "--name-only", &base.sha, "HEAD"],
            )
        })
        .transpose()?
        .unwrap_or_default();

    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status,
        source: promotion_source(source_kind, outcome, options),
        to_worktree: options.to_worktree.clone(),
        target,
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: artifact.map(|a| a.id.clone()).unwrap_or_default(),
            kind: artifact.map_or("patch", |a| a.kind.as_str()).to_string(),
            path: String::new(),
            sha256: artifact.and_then(|a| a.sha256.clone()),
        },
        changed_files,
        command_evidence: Vec::new(),
        deterministic_gates: gates.deterministic_gates,
        gate_results: gates.gate_results,
        verified_base,
        repository_integrity_evidence: options.repository_integrity_evidence.clone(),
        provenance: json!({
            "source_schema": outcome.schema,
            "artifact_metadata": artifact.map(|a| &a.metadata),
            "worktree_path": worktree_path,
            "verified_revision": verified_revision,
            "dependencies_materialized": gates.dependencies_materialized,
            "candidate_checkout_setup": gates.candidate_setup,
            "destination_gate_setup": gates.destination_gate_setup,
            "candidate_checkout": gates.candidate_checkout,
            "candidate": candidate,
        }),
        operator_notification,
    })
}

fn has_candidate_adoption_recovery_eligibility(outcome: &AgentTaskOutcome) -> bool {
    let eligibility = outcome
        .metadata
        .get("candidate_adoption_recovery")
        .or_else(|| outcome.outputs.get("candidate_adoption_recovery"));
    eligibility
        .filter(|value| {
            crate::agent_task_lifecycle::candidate_adoption_recovery_eligibility(value).is_some()
        })
        .is_some()
}

/// An outcome whose change exists only as a patch artifact predates candidate
/// commits (#16303). There is no compatibility reader: it is not promotable.
pub(crate) fn patch_only_outcome_error(outcome: &AgentTaskOutcome) -> Option<Error> {
    if outcome.candidate.is_some() || !outcome.artifacts.iter().any(is_actionable_patch_artifact) {
        return None;
    }
    let mut error = Error::validation_invalid_argument(
        "source",
        "outcome carries a patch artifact but no harvested candidate commit; records written before candidate commits are not promotable (#16303)",
        Some(outcome.task_id.clone()),
        None,
    );
    error.details["recoverable"] = json!(false);
    Some(error)
}

fn valid_sha256(value: &str) -> bool {
    content_hash::is_sha256_hex(value)
}

#[cfg(test)]
mod declared_base_tests {
    use super::*;

    #[test]
    fn promotion_gate_run_dir_disposes_success_and_failure_explicitly() {
        let _guard = homeboy_core::test_support::home_env_guard();
        let root = tempfile::tempdir().expect("runtime root");
        std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", root.path());

        let success = homeboy_core::engine::run_dir::RunDir::create().expect("success run");
        let success_path = success.path().to_path_buf();
        finish_promotion_gate_run_dir(&success, true);
        assert!(!success_path.exists());

        let failure = homeboy_core::engine::run_dir::RunDir::create().expect("failure run");
        let failure_path = failure.path().to_path_buf();
        finish_promotion_gate_run_dir(&failure, false);
        assert!(failure_path.exists());
        std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR");
    }

    use homeboy_core::test_support::run_git_command as git;

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git runs");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn declared_base_capture_is_immune_to_unrelated_fetch_head_activity() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("base"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "main"]);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        let main_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        git(repo.path(), &["checkout", "-b", "other"]);
        std::fs::write(repo.path().join("other"), "other").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "other"]);
        git(repo.path(), &["push", "origin", "other"]);
        git(repo.path(), &["checkout", "main"]);

        let captured = capture_declared_base(repo.path(), Some("main"))
            .expect("capture main")
            .expect("declared base");
        git(repo.path(), &["fetch", "origin", "refs/heads/other"]);

        assert_eq!(captured.base, "main");
        assert_eq!(captured.sha, main_sha);
        assert!(git_output(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/homeboy/promotion/base"
            ],
        )
        .is_empty());
    }

    /// #15017: a PR branch ahead of a base that advanced during the Cook runs
    /// its gates on the fork point, not on the live tip promotion observed.
    #[test]
    fn gate_verified_base_is_where_the_destination_meets_the_moved_base() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("shared"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "main"]);
        let fork = git_output(repo.path(), &["rev-parse", "HEAD"]);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["checkout", "-b", "pr"]);
        std::fs::write(repo.path().join("shared"), "pr").unwrap();
        git(repo.path(), &["commit", "-am", "existing PR commit"]);
        git(repo.path(), &["checkout", "main"]);
        std::fs::write(repo.path().join("shared"), "advanced").unwrap();
        git(repo.path(), &["commit", "-am", "advance main"]);
        git(repo.path(), &["push", "origin", "main"]);
        git(repo.path(), &["checkout", "pr"]);

        let verified = capture_gate_verified_base(repo.path(), Some("main"))
            .expect("capture main")
            .expect("declared base");

        assert_eq!(verified.base, "main");
        assert_eq!(verified.sha, fork);
    }

    #[cfg(unix)]
    fn write_git_fixture(dir: &Path, script: &str) -> PathBuf {
        let git = dir.join("git");
        homeboy_test_support::write_executable(&git, script, 0o755).expect("write Git fixture");

        git
    }

    #[cfg(unix)]
    #[test]
    fn declared_base_transport_failure_is_retryable_and_preserves_redacted_proxy_requirements() {
        let fixture_dir = tempfile::tempdir().expect("fixture directory");
        let git = write_git_fixture(
            fixture_dir.path(),
            "#!/bin/sh\ntest \"$HTTPS_PROXY\" = socks5://proxy.example.test:8080 || exit 91\nprintf '%s\\n' 'fatal: unable to access https://git-user:git-secret@proxy.example.test/repository: Failed to connect to proxy' >&2\nexit 1\n",
        );

        let error = capture_declared_base_with_git_and_timeout(
            fixture_dir.path(),
            Some("main"),
            git.to_str().expect("utf8 fixture path"),
            &[(
                "HTTPS_PROXY".to_string(),
                "socks5://proxy.example.test:8080".to_string(),
            )],
            Duration::from_secs(5),
        )
        .expect_err("transport failure");

        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.code.as_str(), "internal.unexpected");
        assert_eq!(
            error.details["git_base_preflight"]["retry_disposition"],
            "retryable_transport_failure"
        );
        assert_eq!(error.details["git_base_preflight"]["subphase"], "ls-remote");
        assert_eq!(
            error.details["git_base_preflight"]["required_environment"],
            json!(["HTTPS_PROXY"])
        );
        assert_eq!(error.details["git_base_preflight"]["proxy_detected"], true);
        assert_eq!(error.details["git_base_preflight"]["proxy_forwarded"], true);
        let stderr = error.details["git_base_preflight"]["git_stderr"]
            .as_str()
            .expect("sanitized git stderr");
        assert!(stderr.contains("Failed to connect to proxy"));
        assert!(error.message.contains("Failed to connect to proxy"));
        assert!(!error.message.contains("git-secret"));
        assert!(!error.message.contains("git-user"));
        assert!(!stderr.contains("git-secret"));
        assert!(!stderr.contains("git-user"));
        assert!(!error.details.to_string().contains("git-secret"));
        assert!(!error.message.contains("resolveing"));
    }

    #[cfg(unix)]
    #[test]
    fn declared_base_timeout_surfaces_subphase_stderr_and_budget() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-b", "main"]);
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://git-user:git-secret@github.example.com/acme/repo.git",
            ],
        );
        let fixture_dir = tempfile::tempdir().expect("fixture directory");
        let git_bin = write_git_fixture(
            fixture_dir.path(),
            "#!/bin/sh\necho 'fatal: unable to access https://git-user:git-secret@github.example.com/acme/repo.git: Connection timed out' >&2\nsleep 0.3\nexec sleep 10\n",
        );

        let started = Instant::now();
        let error = capture_declared_base_with_git_and_timeout(
            repo.path(),
            Some("main"),
            git_bin.to_str().expect("utf8 fixture path"),
            &[],
            Duration::from_millis(800),
        )
        .expect_err("bounded transport timeout");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(error.retryable, Some(true));
        let preflight = &error.details["git_base_preflight"];
        assert_eq!(preflight["phase"], "resolve");
        assert_eq!(preflight["subphase"], "ls-remote");
        assert_eq!(preflight["timeout_ms"], 800);
        let elapsed_ms = preflight["elapsed_ms"].as_u64().expect("elapsed_ms");
        assert!(elapsed_ms >= 300, "elapsed_ms={elapsed_ms}");
        assert!(elapsed_ms < 3_000, "elapsed_ms={elapsed_ms}");
        assert_eq!(preflight["base_ref"], "main");
        assert_eq!(preflight["proxy_detected"], false);
        assert_eq!(preflight["proxy_forwarded"], false);
        let remote = preflight["remote"].as_str().expect("remote");
        assert!(remote.contains("github.example.com/acme/repo.git"));
        assert!(!remote.contains("git-secret"));
        assert!(!remote.contains("git-user"));
        let stderr = preflight["git_stderr"]
            .as_str()
            .expect("sanitized git stderr");
        assert!(stderr.contains("Connection timed out"));
        assert!(error.message.contains("Connection timed out"));
        assert!(!stderr.contains("git-secret"));
        assert!(!stderr.contains("git-user"));
        assert!(error.message.contains("ls-remote"));
        assert!(error.message.contains("800ms"));
        assert!(!error.message.contains("resolveing"));
        assert!(!error.message.contains("git-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn declared_base_configured_deadline_allows_slower_healthy_resolve() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-b", "main"]);
        let fixture_dir = tempfile::tempdir().expect("fixture directory");
        let git_bin = write_git_fixture(
            fixture_dir.path(),
            "#!/bin/sh\ncase \"$1\" in\nls-remote)\n  sleep 0.2\n  printf '%s\\n' '0123456789abcdef0123456789abcdef01234567\trefs/heads/main'\n  ;;\nfetch) ;;\nrev-parse) printf '%s\\n' '0123456789abcdef0123456789abcdef01234567' ;;\n*) exit 2 ;;\nesac\n",
        );

        let captured = capture_declared_base_with_git_and_timeout(
            repo.path(),
            Some("main"),
            git_bin.to_str().expect("utf8 fixture path"),
            &[],
            Duration::from_millis(1_000),
        )
        .expect("healthy resolve within configured budget")
        .expect("declared base");

        assert_eq!(captured.base, "main");
        assert_eq!(captured.sha, "0123456789abcdef0123456789abcdef01234567");
    }

    #[cfg(unix)]
    #[test]
    fn declared_base_configured_deadline_still_fires() {
        let fixture_dir = tempfile::tempdir().expect("fixture directory");
        let git = write_git_fixture(fixture_dir.path(), "#!/bin/sh\nsleep 2\n");

        let started = Instant::now();
        let error = capture_declared_base_with_git_and_timeout(
            fixture_dir.path(),
            Some("main"),
            git.to_str().expect("utf8 fixture path"),
            &[],
            Duration::from_millis(100),
        )
        .expect_err("bounded transport timeout");

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["git_base_preflight"]["timeout_ms"], 100);
        assert_eq!(error.details["git_base_preflight"]["subphase"], "ls-remote");
        let elapsed_ms = error.details["git_base_preflight"]["elapsed_ms"]
            .as_u64()
            .expect("elapsed_ms");
        assert!(elapsed_ms >= 100, "elapsed_ms={elapsed_ms}");
    }

    #[test]
    fn declared_base_git_timeout_honours_operator_override() {
        let _guard =
            homeboy_core::test_support::EnvVarGuard::set(DECLARED_BASE_GIT_TIMEOUT_ENV, "45");
        assert_eq!(declared_base_git_timeout(), Duration::from_secs(45));
        drop(_guard);
        let _zero =
            homeboy_core::test_support::EnvVarGuard::set(DECLARED_BASE_GIT_TIMEOUT_ENV, "0");
        assert_eq!(
            declared_base_git_timeout(),
            DEFAULT_DECLARED_BASE_GIT_TIMEOUT
        );
    }

    #[cfg(unix)]
    #[test]
    fn declared_base_github_host_proxy_is_forwarded_to_spawned_git() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _https = homeboy_core::test_support::EnvVarGuard::unset("HTTPS_PROXY");
            let _https_lc = homeboy_core::test_support::EnvVarGuard::unset("https_proxy");
            let _http = homeboy_core::test_support::EnvVarGuard::unset("HTTP_PROXY");
            let _http_lc = homeboy_core::test_support::EnvVarGuard::unset("http_proxy");
            let _all = homeboy_core::test_support::EnvVarGuard::unset("ALL_PROXY");
            let _all_lc = homeboy_core::test_support::EnvVarGuard::unset("all_proxy");

            let config_dir = home.path().join(".config/homeboy");
            std::fs::create_dir_all(&config_dir).expect("config dir");
            std::fs::write(
                config_dir.join("homeboy.json"),
                r#"{"github_hosts":{"github.example.com":{"proxy":"socks5://127.0.0.1:8080"}}}"#,
            )
            .expect("write host proxy config");
            homeboy_core::defaults::reset_config_cache_for_test();

            let repo = tempfile::tempdir().expect("repo");
            git(repo.path(), &["init", "-b", "main"]);
            git(
                repo.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.example.com/acme/repo.git",
                ],
            );

            let env = transport_environment(repo.path());
            assert!(
                env.contains(&(
                    "HTTPS_PROXY".to_string(),
                    "socks5://127.0.0.1:8080".to_string()
                )),
                "host proxy must be collected for git: {env:?}"
            );

            let fixture_dir = tempfile::tempdir().expect("fixture directory");
            let git_bin = write_git_fixture(
                fixture_dir.path(),
                "#!/bin/sh\ntest \"$HTTPS_PROXY\" = socks5://127.0.0.1:8080 || exit 91\nprintf '%s\\n' 'fatal: unable to access https://github.example.com/acme/repo.git: Failed to connect to proxy' >&2\nexit 1\n",
            );

            let error = capture_declared_base_with_git_and_timeout(
                repo.path(),
                Some("main"),
                git_bin.to_str().expect("utf8 fixture path"),
                &env,
                Duration::from_millis(250),
            )
            .expect_err("proxy-aware transport failure");

            assert_eq!(error.retryable, Some(true));
            assert_eq!(
                error.details["git_base_preflight"]["retry_disposition"],
                "retryable_transport_failure"
            );
            assert_eq!(error.details["git_base_preflight"]["proxy_detected"], true);
            assert_eq!(error.details["git_base_preflight"]["proxy_forwarded"], true);
            assert_ne!(
                error.details["git_base_preflight"]["required_environment"],
                json!([])
            );
        });
    }

    /// An SSH credential failure is transport evidence in its own right. It
    /// used to be classified only because the remote host string contained
    /// `proxy`, so the same failure to any other host was misreported as an
    /// invalid base ref.
    #[test]
    fn ssh_credential_failure_is_transport_without_a_proxy_hostname() {
        assert!(is_git_transport_failure(
            "git@github.example: Permission denied (publickey).\nfatal: Could not read from remote repository."
        ));
        assert!(is_git_transport_failure(
            "sign_and_send_pubkey: signing failed for ECDSA from agent: agent refused operation"
        ));
        assert!(is_git_transport_failure("Host key verification failed."));
    }

    /// A hostname is not evidence. A genuinely invalid ref must stay a
    /// validation failure even when the remote is named `proxy.example.test`.
    #[test]
    fn a_proxy_hostname_alone_is_not_transport_evidence() {
        assert!(!is_git_transport_failure(
            "fatal: couldn't find remote ref refs/heads/nope on proxy.example.test"
        ));
    }

    #[test]
    fn materialized_workspace_is_the_lab_execution_checkout() {
        assert!(materialized_promotion_workspace_in_context(None)
            .expect("local native promotion")
            .is_none());

        let workspace =
            materialized_promotion_workspace_in_context(Some("runner-1")).expect("runner checkout");
        assert_eq!(
            workspace,
            std::env::current_dir().and_then(std::fs::canonicalize).ok()
        );
    }
}

fn run_promotion_gates(
    options: &AgentTaskPromotionRequest,
    worktree_path: &Path,
    expected_candidate: Option<&crate::agent_task_promotion::AgentTaskPromotionCandidate>,
    committed_candidate: Option<&crate::agent_task::AgentTaskCandidate>,
    gate_workspace: Option<&Path>,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<PromotionGateRun> {
    let operation = || {
        run_promotion_gates_inner(
            options,
            worktree_path,
            expected_candidate,
            committed_candidate,
            gate_workspace,
            observation_store,
        )
    };
    if let Some(run_id) = options.source_run_id.as_deref() {
        observation_store.time_run_phase_with_outcome(run_id, "gate", operation, |gates| {
            gates.status.gate_failed()
        })
    } else {
        operation()
    }
}

fn run_promotion_gates_inner(
    options: &AgentTaskPromotionRequest,
    worktree_path: &Path,
    expected_candidate: Option<&crate::agent_task_promotion::AgentTaskPromotionCandidate>,
    committed_candidate: Option<&crate::agent_task::AgentTaskCandidate>,
    gate_workspace: Option<&Path>,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<PromotionGateRun> {
    emit_promotion_progress(
        "hydration",
        None,
        Some("preparing candidate and dependencies".to_string()),
    );
    if worktree_path.is_dir() {
        homeboy_core::repository_integrity::verify_tracked_symlink_portability(
            worktree_path,
            "HEAD",
            options.repository_integrity_evidence.as_ref(),
        )?;
    }
    if options.dry_run
        || (options.gates.verify.is_empty()
            && options.gates.private_verify.is_empty()
            && options.gates.test_execution_plan.is_none())
    {
        return Ok(PromotionGateRun::without_gates(options.dry_run));
    }

    let candidate_checkout = ImmutableCandidateCheckout::materialize(
        worktree_path,
        options
            .source_run_id
            .as_deref()
            .unwrap_or("unrecorded-promotion"),
        expected_candidate,
        committed_candidate,
        observation_store,
    )?;
    // The immutable candidate establishes source identity only. Dependency
    // setup is intentionally destination-only: gates must consume the exact
    // executable projection this report describes.
    let candidate_setup = Vec::new();
    let gate_workspace = promotion_gate_workspace(options, worktree_path, gate_workspace);
    let lab_placement = options.gates.gate_environment.lab_runner.is_some();
    let destination_gate_setup = if lab_placement {
        // A Lab gate runs in the runner's materialized workspace, so the
        // controller checkout is never hydrated for Lab placement: the runner
        // hydrates that workspace before the gate command, and its receipts
        // carry the setup evidence that this report projects (#16262).
        Vec::new()
    } else if gate_workspace.is_dir() {
        // Local setup runs under the gates' environment (#16024).
        let gate_hydration = Some(crate::agent_task_gate::GateHydrationEnvironment::prepare(
            &options.gates.gate_environment,
        ))
        .transpose()?;
        let hydration_policy = homeboy_core::deps::DependencyHydrationPolicy {
            timeout: options.gates.gate_timeout(),
            no_progress_timeout: options.gates.gate_no_progress_timeout(),
            heartbeat_interval: options.gates.gate_heartbeat_interval(),
            is_cancelled: promotion_cancellation(),
            on_progress: Arc::new(|progress| {
                emit_promotion_progress(
                    "hydration",
                    None,
                    Some(format!(
                        "provider={} phase={} elapsed={}ms last-progress={}ms",
                        progress.provider_id,
                        progress.phase,
                        progress.elapsed_ms,
                        progress.last_progress_ms_ago.unwrap_or(progress.elapsed_ms),
                    )),
                );
            }),
            environment: gate_hydration
                .as_ref()
                .map(|hydration| hydration.environment())
                .unwrap_or_default(),
        };
        // Detached gate workspaces have no submodules until initialized
        // (#15355). Recorded with the setup evidence; a failure here does not
        // fail setup on its own, since gates that need a missing submodule
        // fail with their own diagnostics.
        let submodule_setup = options
            .gates
            .hydrate_dependencies
            .then(|| {
                homeboy_core::deps::hydrate_git_submodules(
                    &gate_workspace,
                    None,
                    "destination_gate_workspace",
                    homeboy_core::deps::SUBMODULE_HYDRATION_TIMEOUT,
                )
            })
            .flatten();
        let setup = crate::agent_task_gate::hydrate_gate_dependency_roots_for_component(
            &gate_workspace,
            options.gates.hydrate_dependencies,
            "destination_gate_workspace",
            &hydration_policy,
            options
                .gates
                .gate_environment
                .admitted_component_id
                .as_deref(),
        )
        .map_err(|error| {
            gate_setup_failure(
                "destination_gate_setup",
                "destination_gate_workspace",
                error,
            )
        });
        if let Some(gate_hydration) = gate_hydration {
            gate_hydration.finish(setup.as_ref().is_ok_and(|setup| {
                setup.iter().all(|outcome| {
                    outcome.status != homeboy_core::deps::DependencyHydrationStatus::Failed
                })
            }));
        }
        let setup = setup?;
        if let Some(failed) = setup
            .iter()
            .find(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed)
        {
            return Err(gate_setup_outcome_failure("destination_gate_setup", failed));
        }
        submodule_setup.into_iter().chain(setup).collect()
    } else {
        // An opaque provider destination cannot be hydrated locally. Its gate
        // provider remains the authority, and the report makes no local
        // materialization claim.
        Vec::new()
    };
    let declared_gates = options
        .gates
        .verify
        .iter()
        .map(|command| {
            (
                command,
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )
        })
        .chain(options.gates.private_verify.iter().map(|command| {
            (
                command,
                AgentTaskGateVisibility::Private,
                options.gates.private_gate_reveal,
            )
        }));
    let mut deterministic_gates = Vec::new();
    let mut blocking_gate_id = None;
    if let Some(plan) = options.gates.test_execution_plan.as_ref() {
        emit_promotion_progress(
            "gate",
            Some("gate-1".to_string()),
            Some("starting declared test plan".to_string()),
        );
        let gate = run_declared_promotion_test(options, &gate_workspace, 1, plan)?;
        if matches!(
            gate.status,
            AgentTaskGateStatus::Failed
                | AgentTaskGateStatus::Deferred
                | AgentTaskGateStatus::Unavailable
        ) && options.gates.execution_policy
            == crate::agent_task_gate::AgentTaskGateExecutionPolicy::OrderedFailFast
        {
            blocking_gate_id = Some(gate.id.clone());
        }
        deterministic_gates.push(gate);
    }
    let legacy_gate_offset = deterministic_gates.len();
    for (index, (command, visibility, reveal_policy)) in declared_gates.enumerate() {
        let index = index + legacy_gate_offset + 1;
        let execution_workspace = if options.gates.gate_environment.lab_runner.is_some()
            || crate::agent_task_gate::placement::declared_runner(command)?.is_some()
        {
            worktree_path
        } else {
            &gate_workspace
        };
        let gate = if let Some(blocking_gate_id) = blocking_gate_id.as_deref() {
            crate::agent_task_gate::AgentTaskGateReport::skipped(
                format!("gate-{index}"),
                crate::agent_task_gate::legacy_gate_argv(command),
                visibility,
                reveal_policy,
                blocking_gate_id,
            )
        } else {
            emit_promotion_progress(
                "gate",
                Some(format!("gate-{index}")),
                Some("starting deterministic gate".to_string()),
            );
            run_promotion_gate(
                options,
                execution_workspace,
                index,
                command,
                visibility,
                reveal_policy,
                observation_store,
            )?
        };
        if matches!(
            gate.status,
            AgentTaskGateStatus::Failed
                | AgentTaskGateStatus::Deferred
                | AgentTaskGateStatus::Unavailable
        ) && options.gates.execution_policy
            == crate::agent_task_gate::AgentTaskGateExecutionPolicy::OrderedFailFast
        {
            blocking_gate_id = Some(gate.id.clone());
        }
        let mut gate = gate;
        gate.cwd = Some(crate::agent_task_gate::AgentTaskGateCwdEvidence {
            requested: worktree_path.display().to_string(),
            effective: execution_workspace.display().to_string(),
        });
        deterministic_gates.push(gate);
    }
    emit_promotion_progress(
        "cleanup",
        None,
        Some("cleaning promotion gate resources".to_string()),
    );
    let has_gate_failure = deterministic_gates
        .iter()
        .any(|gate| gate.status == AgentTaskGateStatus::Failed);
    let has_gate_deferral = deterministic_gates.iter().any(|gate| {
        matches!(
            gate.status,
            AgentTaskGateStatus::Deferred | AgentTaskGateStatus::Unavailable
        )
    });
    let gate_results = deterministic_gates
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();
    // The runner owns dependency setup for Lab placement: project the setup
    // evidence its gate receipts recorded so the promotion report still names
    // where dependencies were materialized — the runner workspace (#16262).
    let destination_gate_setup = if lab_placement {
        deterministic_gates
            .iter()
            .flat_map(|gate| gate.setup.iter().cloned())
            .collect()
    } else {
        destination_gate_setup
    };

    let candidate_checkout = candidate_checkout.map(|checkout| checkout.identity());
    if let Some(candidate_checkout) = &candidate_checkout {
        for gate in &mut deterministic_gates {
            bind_lab_gate_candidate_checkout(gate, candidate_checkout)?;
        }
    }
    Ok(PromotionGateRun {
        status: status_for_report(options.dry_run, has_gate_failure, has_gate_deferral),
        deterministic_gates,
        gate_results,
        dependencies_materialized: destination_gate_setup
            .iter()
            .any(|setup| setup.status == homeboy_core::deps::DependencyHydrationStatus::Succeeded),
        candidate_setup,
        destination_gate_setup,
        candidate_checkout,
    })
}

fn bind_lab_gate_candidate_checkout(
    gate: &mut crate::agent_task_gate::AgentTaskGateReport,
    candidate: &crate::agent_task_gate::AgentTaskGateCandidateCheckout,
) -> Result<()> {
    use crate::agent_task_gate::placement::{LAB_GATE_DISPOSITION_SCHEMA, LAB_GATE_RECEIPT_SCHEMA};
    if let Some(receipt) = &gate.lab_receipt {
        match receipt.get("schema").and_then(Value::as_str) {
            Some(LAB_GATE_RECEIPT_SCHEMA) => {
                if receipt.pointer("/candidate/fingerprint/tree").and_then(Value::as_str) != Some(candidate.tree.as_str()) {
                    return Err(Error::invalid_argument("gate.candidate", "Lab gate terminal proof differs from the canonical promotion destination tree"));
                }
            }
            Some(LAB_GATE_DISPOSITION_SCHEMA) if receipt.get("executed").and_then(Value::as_bool) == Some(false)
                && !matches!(gate.status, AgentTaskGateStatus::Succeeded | AgentTaskGateStatus::AcceptedInheritedFailure) => {}
            _ => return Err(Error::invalid_argument("gate.receipt", "Lab gate evidence has no recognized non-passing disposition or admitted candidate receipt")),
        }
    }
    gate.candidate_checkout = Some(candidate.clone());
    Ok(())
}

#[cfg(test)]
mod lab_gate_binding_tests {
    use super::*;

    #[test]
    fn runner_report_hands_back_the_candidate_commit_and_its_bundle() {
        let _home = homeboy_core::test_support::home_env_guard();
        let runner_env = homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
        let mut report: AgentTaskPromotionReport = serde_json::from_value(json!({
            "status": "gate_failed",
            "source": { "kind": "aggregate", "task_id": "task-1" },
            "to_worktree": "repo@fix",
            "target": { "worktree": "repo@fix" },
            "patch_artifact": { "id": "patch", "kind": "patch", "path": "p.patch" },
            "operator_notification": { "status": "failed", "message": "gate failed" },
        }))
        .expect("report");
        let candidate = json!({ "base_sha": "base", "candidate_sha": "c", "tree_sha": "t" });
        let bundle = json!({
            "id": "task-1-attempt-1-candidate-bundle",
            "kind": crate::agent_task::CANDIDATE_BUNDLE_ARTIFACT_KIND,
            "metadata": { "run_id": "run-1" },
        });
        let options = |artifacts: Value| AgentTaskPromotionRequest {
            source: json!({
                "schema": crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": "task-1",
                "status": "succeeded",
                "candidate": candidate,
                "artifacts": artifacts,
            })
            .to_string(),
            source_run_id: None,
            source_path: None,
            source_worktree_path: None,
            to_worktree: "repo@fix".to_string(),
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: Default::default(),
            provider_invocation: None,
            repository_integrity_evidence: None,
        };
        let handback = lab_candidate_handback(&options(json!([bundle])));
        assert_eq!(
            handback,
            json!({
                "run_id": "run-1",
                "task_id": "task-1",
                "artifact_id": "task-1-attempt-1-candidate-bundle",
                "candidate": candidate,
            })
        );
        assert!(lab_candidate_handback(&options(json!([]))).is_null());

        let _local = homeboy_core::test_support::EnvVarGuard::unset(runner_env);
        stamp_lab_offload(&mut report, handback.clone());
        assert!(report.provenance.get("lab_offload").is_none());
        let _runner = homeboy_core::test_support::EnvVarGuard::set(runner_env, "lab-1");
        stamp_lab_offload(&mut report, handback.clone());
        assert_eq!(report.provenance["lab_offload"]["runner_id"], "lab-1");
        assert_eq!(
            report.provenance["lab_offload"]["candidate_handback"],
            handback
        );
    }

    fn with_git_candidate(test: impl FnOnce(&Path, &homeboy_core::observation::ObservationStore)) {
        homeboy_core::test_support::with_isolated_home(|home| {
            // Root all four owners explicitly. `with_isolated_home` already
            // clears inherited roots; these fixtures pin them deliberately.
            let _config = homeboy_core::test_support::EnvVarGuard::set(
                "HOMEBOY_CONFIG_ROOT",
                home.path().join(".config/homeboy"),
            );
            let _data = homeboy_core::test_support::EnvVarGuard::set(
                "HOMEBOY_DATA_DIR",
                home.path().join("data"),
            );
            let _runtime = homeboy_core::test_support::EnvVarGuard::set(
                "HOMEBOY_RUNTIME_TMPDIR",
                home.path().join("runtime"),
            );
            let _artifacts = homeboy_core::test_support::EnvVarGuard::set(
                "HOMEBOY_ARTIFACT_ROOT",
                home.path().join("artifacts"),
            );
            let repo = home.path().join("candidate");
            std::fs::create_dir_all(&repo).unwrap();
            for args in [
                vec!["init", "-q"],
                vec!["config", "user.name", "Fixture"],
                vec!["config", "user.email", "fixture@example.invalid"],
            ] {
                homeboy_core::test_support::run_git_fixture_command(&repo, &args);
            }
            std::fs::write(repo.join("candidate.txt"), "candidate\n").unwrap();
            homeboy_core::test_support::run_git_fixture_command(&repo, &["add", "."]);
            homeboy_core::test_support::run_git_fixture_command(
                &repo,
                &["commit", "-qm", "candidate"],
            );
            let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
            test(&repo, &store);
        });
    }

    #[test]
    fn unavailable_lab_promotion_retains_checkout_and_blocks_ordered_successors() {
        with_git_candidate(|repo, store| {
            let candidate =
                crate::agent_task_promotion::candidate_fingerprint(&repo.display().to_string())
                    .unwrap();
            let options: AgentTaskPromotionRequest = serde_json::from_value(json!({
                "source":"{}", "to_worktree":"fixture", "gates":{
                    "verify":["printf must-not-run", "printf successor-must-not-run"], "hydrate_dependencies":false,
                    "gate_environment":{"lab_runner":"absent-fixture-runner", "hydrate_rust_cache":false}
                }
            })).unwrap();
            let report = run_promotion_gates(&options, repo, Some(&candidate), None, None, store)
                .expect("non-execution is retained, not a fabricated candidate mismatch");
            assert_eq!(report.status, AgentTaskPromotionStatus::GateDeferred);
            assert_eq!(report.deterministic_gates.len(), 2);
            let unavailable = &report.deterministic_gates[0];
            assert_eq!(unavailable.status, AgentTaskGateStatus::Unavailable);
            assert!(unavailable.stdout.is_empty());
            assert_eq!(unavailable.lab_receipt.as_ref().unwrap()["executed"], false);
            assert!(unavailable.candidate_checkout.is_some());
            assert_eq!(
                report.gate_results[0].status,
                homeboy_core::gate::HomeboyGateStatus::Blocked
            );
            assert_eq!(
                report.deterministic_gates[1].status,
                AgentTaskGateStatus::Skipped
            );
            assert_eq!(
                report.deterministic_gates[1]
                    .skip_reason
                    .as_ref()
                    .unwrap()
                    .blocking_gate_id,
                unavailable.id
            );
            assert_eq!(
                report.candidate_checkout.as_ref().unwrap().tree,
                unavailable.candidate_checkout.as_ref().unwrap().tree
            );
        });
    }

    /// #16262: with Lab placement, the controller checkout is never hydrated
    /// for gate setup — the runner hydrates its own gate workspace, and this
    /// report carries the runner's setup evidence instead.
    #[test]
    fn lab_placement_performs_no_controller_side_dependency_setup() {
        with_git_candidate(|repo, store| {
            std::fs::write(
                repo.join("homeboy.json"),
                r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#,
            )
            .unwrap();
            homeboy_core::test_support::run_git_fixture_command(repo, &["add", "."]);
            homeboy_core::test_support::run_git_fixture_command(
                repo,
                &["commit", "-qm", "dependency manifest"],
            );
            let candidate =
                crate::agent_task_promotion::candidate_fingerprint(&repo.display().to_string())
                    .unwrap();
            let options: AgentTaskPromotionRequest = serde_json::from_value(json!({
                "source":"{}", "to_worktree":"fixture", "gates":{
                    "verify":["printf must-not-run"],
                    "gate_environment":{"lab_runner":"absent-fixture-runner", "hydrate_rust_cache":false}
                }
            }))
            .unwrap();

            let report = run_promotion_gates(&options, repo, Some(&candidate), None, None, store)
                .expect("an absent runner defers the gate; it is not a setup error");

            assert_eq!(report.status, AgentTaskPromotionStatus::GateDeferred);
            assert!(
                !repo.join("hydration-count").exists(),
                "controller-side dependency setup must not touch the controller checkout for Lab placement"
            );
            assert!(
                report.destination_gate_setup.is_empty(),
                "controller setup evidence stays empty; the runner reports its own"
            );
            assert_eq!(
                report.deterministic_gates[0].status,
                AgentTaskGateStatus::Unavailable
            );
        });
    }

    /// The control for `lab_placement_performs_no_controller_side_dependency_setup`:
    /// the same manifest under local placement is hydrated in the destination
    /// gate workspace and recorded as its setup evidence.
    #[test]
    fn local_placement_still_hydrates_the_destination_gate_workspace() {
        with_git_candidate(|repo, store| {
            std::fs::write(
                repo.join("homeboy.json"),
                r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#,
            )
            .unwrap();
            homeboy_core::test_support::run_git_fixture_command(repo, &["add", "."]);
            homeboy_core::test_support::run_git_fixture_command(
                repo,
                &["commit", "-qm", "dependency manifest"],
            );
            let candidate =
                crate::agent_task_promotion::candidate_fingerprint(&repo.display().to_string())
                    .unwrap();
            let options: AgentTaskPromotionRequest = serde_json::from_value(json!({
                "source":"{}", "to_worktree":"fixture", "gates":{
                    "verify":["printf must-not-run"],
                    "gate_environment":{"hydrate_rust_cache":false}
                }
            }))
            .unwrap();

            let report = run_promotion_gates(&options, repo, Some(&candidate), None, None, store)
                .expect("local gates run against the destination workspace");

            assert!(repo.join("hydration-count").is_file());
            assert!(report
                .destination_gate_setup
                .iter()
                .any(|setup| setup.status
                    == homeboy_core::deps::DependencyHydrationStatus::Succeeded));
            assert!(report.dependencies_materialized);
        });
    }

    /// Commit the destination's dirty tree on HEAD the way harvest does, so
    /// the candidate exists in the destination repository before promotion.
    fn harvested_candidate(repo: &Path) -> crate::agent_task::AgentTaskCandidate {
        let index = tempfile::NamedTempFile::new().unwrap();
        let git_with_index = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .env("GIT_INDEX_FILE", index.path())
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git_with_index(&["read-tree", "HEAD"]);
        git_with_index(&["add", "--all"]);
        let tree_sha = git_with_index(&["write-tree"]);
        let base_sha = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
        let candidate_sha = git_output(
            repo,
            &["commit-tree", &tree_sha, "-p", &base_sha, "-m", "harvested"],
        )
        .unwrap();
        git_output(
            repo,
            &[
                "update-ref",
                "refs/homeboy/candidates/fixture",
                &candidate_sha,
            ],
        )
        .unwrap();
        crate::agent_task::AgentTaskCandidate {
            base_sha,
            candidate_sha,
            tree_sha,
        }
    }

    fn commit_objects(repo: &Path) -> std::collections::BTreeSet<String> {
        git_output(
            repo,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objecttype) %(objectname)",
            ],
        )
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("commit ").map(str::to_string))
        .collect()
    }

    #[test]
    fn matching_harvested_candidate_is_the_gate_checkout_without_a_new_commit() {
        with_git_candidate(|repo, store| {
            std::fs::write(repo.join("candidate.txt"), "changed\n").unwrap();
            std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
            let candidate = harvested_candidate(repo);
            let commits_before = commit_objects(repo);

            let checkout = ImmutableCandidateCheckout::materialize(
                repo,
                "candidate-fixture",
                None,
                Some(&candidate),
                store,
            )
            .unwrap()
            .expect("Git destination materializes a gate checkout");

            assert_eq!(
                git_output(&checkout.path, &["rev-parse", "HEAD"]).unwrap(),
                candidate.candidate_sha
            );
            assert_eq!(checkout.identity().commit, candidate.candidate_sha);
            assert_eq!(checkout.identity().tree, candidate.tree_sha);
            assert_eq!(commit_objects(repo), commits_before);
        });
    }

    #[test]
    fn mismatching_harvested_candidate_tree_fails_with_typed_error() {
        with_git_candidate(|repo, store| {
            std::fs::write(repo.join("candidate.txt"), "changed\n").unwrap();
            let candidate = harvested_candidate(repo);
            std::fs::write(repo.join("candidate.txt"), "diverged\n").unwrap();

            let error = ImmutableCandidateCheckout::materialize(
                repo,
                "candidate-fixture",
                None,
                Some(&candidate),
                store,
            )
            .err()
            .expect("a diverged destination tree must not be gated");

            let Some(CandidateAdmissionError::TreeMismatch {
                candidate_sha,
                candidate_tree,
                actual_tree: destination_tree,
            }) = std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<CandidateAdmissionError>())
            else {
                panic!("typed candidate tree mismatch: {error:?}");
            };
            assert_eq!(candidate_sha, &candidate.candidate_sha);
            assert_eq!(candidate_tree, &candidate.tree_sha);
            assert_ne!(destination_tree, &candidate.tree_sha);
            assert!(error.message.contains(&candidate.tree_sha));
            assert!(error.message.contains(destination_tree.as_str()));
        });
    }

    #[test]
    fn executed_lab_tree_mismatch_is_rejected_at_real_checkout_binding_boundary() {
        with_git_candidate(|repo, store| {
            let checkout = ImmutableCandidateCheckout::materialize(
                repo,
                "lab-binding-fixture",
                None,
                None,
                store,
            )
            .unwrap()
            .unwrap();
            let environment = crate::agent_task_gate::AgentTaskGateEnvironmentPolicy {
                hydrate_rust_cache: false,
                ..Default::default()
            };
            let mut gate = crate::agent_task_gate::run_gate_command_with_supervision(
                repo,
                1,
                "printf executed",
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                None,
                &environment,
                &[],
            )
            .unwrap();
            assert_eq!(gate.stdout, "executed");
            // Deliberately corrupt proof at this consumer boundary. This test
            // does not claim synthetic admitted receipts as transport evidence.
            gate.lab_receipt = Some(
                json!({"schema":crate::agent_task_gate::placement::LAB_GATE_RECEIPT_SCHEMA,
                "candidate":{"fingerprint":{"tree":"mismatched-executed-tree"}}}),
            );
            let error =
                bind_lab_gate_candidate_checkout(&mut gate, &checkout.identity()).unwrap_err();
            assert_eq!(error.details["field"], "gate.candidate");
            gate.lab_receipt = Some(
                json!({"schema":crate::agent_task_gate::placement::LAB_GATE_DISPOSITION_SCHEMA, "executed":false}),
            );
            assert!(
                bind_lab_gate_candidate_checkout(&mut gate, &checkout.identity()).is_err(),
                "non-execution evidence cannot make an executed passing gate green"
            );
        });
    }
}

fn run_declared_promotion_test(
    options: &AgentTaskPromotionRequest,
    worktree_path: &Path,
    index: usize,
    plan: &homeboy_engine_primitives::test_execution::TestExecutionPlan,
) -> Result<crate::agent_task_gate::AgentTaskGateReport> {
    let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
    let runtime_tmpdir = homeboy_core::engine::invocation::InvocationGuard::acquire(
        &run_dir,
        &homeboy_core::engine::invocation::InvocationRequirements::default(),
    )?;
    let mut gate_environment = options.gates.gate_environment.clone();
    gate_environment.hydrate_rust_cache &= options.gates.hydrate_dependencies;
    gate_environment.hydrate_dependencies &= options.gates.hydrate_dependencies;
    let supervision = GATE_SUPERVISION.with(|slot| slot.borrow().clone());
    let timeout = plan.suite_timeout();
    let supervision = supervision.map(|supervision| crate::agent_task_gate::GateSupervision {
        timeout,
        no_progress_timeout: supervision.no_progress_timeout,
        heartbeat_interval: supervision.heartbeat_interval,
        on_spawn: supervision.on_spawn.clone(),
        on_heartbeat: supervision.on_heartbeat.clone(),
        is_cancelled: supervision.is_cancelled.clone(),
    });
    let fallback_supervision = crate::agent_task_gate::GateSupervision {
        timeout,
        no_progress_timeout: timeout,
        heartbeat_interval: Duration::from_secs(5),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let result = crate::agent_task_gate::run_declared_test_with_supervision(
        worktree_path,
        index,
        plan,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(&runtime_tmpdir.context().tmp_dir),
        supervision.as_ref().or(Some(&fallback_supervision)),
        &gate_environment,
        &options.gates.gate_package_artifacts,
    );
    finish_promotion_gate_run_dir(&run_dir, result.is_ok());
    result
}

fn gate_workspace_path(options: &AgentTaskPromotionRequest, worktree_path: &Path) -> PathBuf {
    options
        .source_worktree_path
        .as_ref()
        .filter(|source| source.is_dir())
        .cloned()
        .unwrap_or_else(|| worktree_path.to_path_buf())
}

fn promotion_gate_workspace(
    options: &AgentTaskPromotionRequest,
    worktree_path: &Path,
    gate_workspace: Option<&Path>,
) -> PathBuf {
    // The caller's admitted component workspace is authoritative for both
    // dependency hydration and deterministic gates, regardless of execution
    // placement. Falling back to the candidate root is only for callers that
    // have no component-scoped workspace.
    gate_workspace
        .map(Path::to_path_buf)
        .unwrap_or_else(|| gate_workspace_path(options, worktree_path))
}

fn bounded_setup_error_text(value: &str) -> String {
    const MAX_BYTES: usize = 4096;
    let mut result = String::new();
    for character in value.chars() {
        if result.len() + character.len_utf8() > MAX_BYTES {
            break;
        }
        result.push(character);
    }
    result
}

/// Retry action for a gate setup failure whose cause cannot change between
/// attempts: a validation error the cause itself marked non-retryable, such as a
/// Lab gate rejected in preflight for a missing runner `secret_env` ref (#16121).
pub(crate) const GATE_SETUP_FIX_CONFIGURATION: &str = "fix_configuration";

fn gate_setup_failure_is_deterministic(error: &Error) -> bool {
    error.retryable == Some(false) && error.code.as_str().starts_with("validation.")
}

fn gate_setup_failure(classification: &str, component: &str, error: Error) -> Error {
    let deterministic = gate_setup_failure_is_deterministic(&error);
    let retry_action = if deterministic {
        GATE_SETUP_FIX_CONFIGURATION
    } else {
        "retry_dependency_hydration"
    };
    let mut failure = Error::dependency_step_failed(
        "promotion.gate_setup",
        component.to_string(),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(serde_json::json!({
            "classification": classification,
            "code": error.code.as_str(),
            "message": bounded_setup_error_text(&homeboy_core::redaction::redact_string(&error.message)),
            "details": bounded_setup_error_text(&homeboy_core::redaction::redact_string(&error.details.to_string())),
            "retry_action": retry_action,
        })),
    );
    if deterministic {
        failure.retryable = Some(false);
    }
    failure
}

pub(crate) fn gate_setup_outcome_failure(
    classification: &str,
    outcome: &homeboy_core::deps::DependencyHydrationOutcome,
) -> Error {
    let mut bounded_outcome = outcome.clone();
    bounded_outcome.cwd = bounded_setup_error_text(&bounded_outcome.cwd);
    bounded_outcome.stdout = bounded_setup_error_text(&bounded_outcome.stdout);
    bounded_outcome.stderr = bounded_setup_error_text(&bounded_outcome.stderr);
    let logs = [
        (!bounded_outcome.stdout.is_empty()).then(|| format!("stdout: {}", bounded_outcome.stdout)),
        (!bounded_outcome.stderr.is_empty()).then(|| format!("stderr: {}", bounded_outcome.stderr)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut error = Error::dependency_step_failed(
        "promotion.gate_setup",
        outcome.provider_id.clone(),
        outcome.exit_code,
        logs,
        Vec::new(),
        Some(outcome.command.join(" ")),
        Some(serde_json::json!({
            "classification": classification,
            "outcome": bounded_outcome,
            "retry_action": "retry_dependency_hydration",
        })),
    );
    // The generic message names only the dependency provider ("component
    // 'pnpm'"), which reads like a missing Homeboy component rather than a
    // missing tool or failed install (#15338). Say what actually happened.
    error.message = format!("{}: {}", error.message, gate_setup_outcome_summary(outcome));
    error
}

/// The stderr line that best explains a failure. Tools commonly print
/// runtime warnings (e.g. Node `DeprecationWarning`) before the real error, so
/// prefer a line that looks like an error, then the last meaningful line.
fn stderr_error_line(stderr: &str) -> Option<&str> {
    let lines = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            !line.contains("DeprecationWarning")
                && !line.contains("--trace-deprecation")
                && !line.contains("--trace-warnings")
        })
        .collect::<Vec<_>>();
    lines
        .iter()
        .copied()
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            line.contains("ERR_") || lower.starts_with("error") || lower.contains(" error")
        })
        .or_else(|| lines.last().copied())
}

/// One-line operator explanation of a failed dependency hydration outcome.
fn gate_setup_outcome_summary(outcome: &homeboy_core::deps::DependencyHydrationOutcome) -> String {
    use homeboy_core::deps::DependencyHydrationTermination as Termination;
    let command = outcome.command.join(" ");
    let program = outcome
        .command
        .first()
        .map(String::as_str)
        .unwrap_or("the dependency command");
    let summary = match outcome.termination {
        Termination::SpawnFailed => format!(
            "`{command}` could not be started; install `{program}` where the Homeboy controller runs gates (or make it available on PATH)"
        ),
        Termination::TimedOut => format!(
            "`{command}` timed out after {}ms",
            outcome.duration_ms
        ),
        Termination::NoProgress => format!(
            "`{command}` stopped making progress after {}ms",
            outcome.duration_ms
        ),
        Termination::Cancelled => format!("`{command}` was cancelled"),
        Termination::OutputValidationFailed => format!(
            "`{command}` finished but did not produce its declared outputs"
        ),
        Termination::ExitFailure | Termination::Completed | Termination::NotStarted => {
            match outcome.exit_code {
                Some(code) => format!("`{command}` exited with status {code}"),
                None => format!("`{command}` failed ({})", outcome.reason),
            }
        }
    };
    let last_output = stderr_error_line(&outcome.stderr)
        .map(|line| format!("stderr: {line}"))
        .or_else(|| {
            outcome
                .stdout
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| format!("stdout: {}", line.trim()))
        });
    let cwd = bounded_setup_error_text(&homeboy_core::redaction::redact_string(&outcome.cwd));
    match last_output {
        Some(output) => format!(
            "{summary}; cwd: {cwd}; last output: {}",
            bounded_setup_error_text(&homeboy_core::redaction::redact_string(&output))
        ),
        None => format!("{summary}; cwd: {cwd}"),
    }
}

/// A run-scoped detached worktree whose commit tree is exactly the promoted
/// candidate. The source destination stays dirty for final commit/push/PR.
struct ImmutableCandidateCheckout {
    source_root: PathBuf,
    path: PathBuf,
    allocation: crate::controller_scratch::ControllerScratchAllocation,
    identity: AgentTaskGateCandidateCheckout,
}

impl ImmutableCandidateCheckout {
    fn materialize(
        source_root: &Path,
        run_id: &str,
        expected_candidate: Option<&crate::agent_task_promotion::AgentTaskPromotionCandidate>,
        committed_candidate: Option<&crate::agent_task::AgentTaskCandidate>,
        observation_store: &homeboy_core::observation::ObservationStore,
    ) -> Result<Option<Self>> {
        if !source_root.is_dir() {
            // An external provider can report an opaque or not-yet-mounted
            // destination. Preserve its existing direct verification path.
            return Ok(None);
        }
        let head = Command::new("git")
            .args(["rev-parse", "--verify", "HEAD^{commit}"])
            .current_dir(source_root)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !head.status.success() {
            // Provider-owned and bootstrap destinations can still be verified,
            // but have no committed Git candidate to materialize.
            return Ok(None);
        }
        let candidate =
            crate::agent_task_promotion::candidate_fingerprint(&source_root.display().to_string())?;
        if let Some(expected_candidate) = expected_candidate {
            if &candidate != expected_candidate {
                return Err(Error::validation_invalid_argument(
                    "promotion.provenance.candidate",
                    "promotion destination differs from the checkpointed candidate before deterministic verification",
                    Some(source_root.display().to_string()),
                    None,
                ));
            }
        }
        let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
            candidate
        else {
            return Ok(None);
        };
        // The harvested commit is this destination's candidate when it is
        // reachable here and descends from the destination HEAD. A destination
        // caught up to a moved base is a different candidate (S2 of #16303).
        let git_succeeds = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(source_root)
                .status()
                .is_ok_and(|status| status.success())
        };
        let reachable = committed_candidate.filter(|committed| {
            git_succeeds(&[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", committed.candidate_sha),
            ]) && git_succeeds(&[
                "merge-base",
                "--is-ancestor",
                &fingerprint.head,
                &committed.candidate_sha,
            ])
        });
        let commit = match reachable {
            // The harvested candidate commit is the gated identity (#16303).
            Some(committed) if committed.tree_sha == fingerprint.tree => {
                committed.candidate_sha.clone()
            }
            Some(committed) => {
                return Err(CandidateAdmissionError::TreeMismatch {
                    candidate_sha: committed.candidate_sha.clone(),
                    candidate_tree: committed.tree_sha.clone(),
                    actual_tree: fingerprint.tree.clone(),
                }
                .into())
            }
            None => git_output(
                source_root,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@localhost",
                    "commit-tree",
                    &fingerprint.tree,
                    "-p",
                    &fingerprint.head,
                    "-m",
                    "homeboy: immutable promotion candidate",
                ],
            )?,
        };
        let data_root = observation_store.data_root().ok_or_else(|| {
            Error::internal_unexpected("promotion observation store has no data root")
        })?;
        let allocation = crate::controller_scratch::allocate_attempt_at(
            data_root,
            run_id,
            "promotion-candidate-checkout",
            "candidate",
            1,
        )?;
        crate::controller_scratch::mark_attempt_ephemeral(&allocation)?;
        let path = allocation.path.clone();
        git_output(
            source_root,
            &[
                "worktree",
                "add",
                "--detach",
                &path.display().to_string(),
                &commit,
            ],
        )?;
        let checkout = Self {
            source_root: source_root.to_path_buf(),
            path,
            allocation,
            identity: AgentTaskGateCandidateCheckout {
                schema: "homeboy/agent-task-gate-candidate-checkout/v1".to_string(),
                commit,
                tree: fingerprint.tree,
                candidate_sha256: fingerprint.sha256,
            },
        };
        let status = git_output(
            &checkout.path,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        if !status.is_empty() {
            return Err(Error::validation_invalid_argument(
                "promotion",
                "immutable candidate checkout is unexpectedly dirty",
                Some(checkout.path.display().to_string()),
                None,
            ));
        }
        Ok(Some(checkout))
    }

    fn identity(&self) -> AgentTaskGateCandidateCheckout {
        self.identity.clone()
    }
}

impl Drop for ImmutableCandidateCheckout {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.source_root)
            .status();
        let _ = crate::controller_scratch::release_attempt(
            &self.allocation,
            "candidate_checkout_removed",
            serde_json::json!({ "commit": self.identity.commit, "tree": self.identity.tree }),
        );
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_root)
            .status();
    }
}

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_promotion_gate(
    options: &AgentTaskPromotionRequest,
    worktree_path: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<crate::agent_task_gate::AgentTaskGateReport> {
    let run_id = options
        .source_run_id
        .as_deref()
        .unwrap_or("unrecorded-promotion");
    let data_root = observation_store.data_root().ok_or_else(|| {
        Error::internal_unexpected("promotion observation store has no data root")
    })?;
    let allocation = crate::controller_scratch::allocate_attempt_at(
        data_root,
        run_id,
        "promotion-verification",
        &format!("gate-{index}"),
        1,
    )?;
    let run_dir = match homeboy_core::engine::run_dir::RunDir::create() {
        Ok(run_dir) => run_dir,
        Err(error) => {
            crate::controller_scratch::release_attempt(
                &allocation,
                "verification_runtime_setup_failed",
                serde_json::json!({ "error": error.message }),
            )?;
            return Err(error);
        }
    };
    let runtime_tmpdir = match homeboy_core::engine::invocation::InvocationGuard::acquire(
        &run_dir,
        &homeboy_core::engine::invocation::InvocationRequirements::default(),
    ) {
        Ok(runtime_tmpdir) => runtime_tmpdir,
        Err(error) => {
            finish_promotion_gate_run_dir(&run_dir, false);
            crate::controller_scratch::release_attempt(
                &allocation,
                "verification_runtime_setup_failed",
                serde_json::json!({ "error": error.message }),
            )?;
            return Err(error);
        }
    };
    let supervision = GATE_SUPERVISION.with(|slot| slot.borrow().clone());
    let mut gate_environment = options.gates.gate_environment.clone();
    // A dependency-hydration opt-out also opts out of controller Rust cache
    // hydration, and of the runner-side hydration a Lab gate performs; the
    // verification then runs solely with its isolated gate state.
    gate_environment.hydrate_rust_cache &= options.gates.hydrate_dependencies;
    gate_environment.hydrate_dependencies &= options.gates.hydrate_dependencies;
    if gate_environment.lab_runner.is_none() {
        gate_environment.lab_runner = crate::agent_task_gate::placement::declared_runner(command)?;
    }
    // Pre-dispatch validation protects provider budget. Recheck in this exact
    // runtime because every gate receives its own isolated HOME/XDG directory.
    let result = if let Err(error) = crate::agent_task_gate::preflight_gate_toolchains(
        worktree_path,
        &gate_environment,
        &options.gates.required_toolchains(),
        &options.gates.gate_package_artifacts,
        Some(&runtime_tmpdir.context().tmp_dir),
        options.gates.gate_timeout(),
    ) {
        // This runs before provider verification. A missing destination tool is
        // setup evidence, not candidate blame or provider-budget consumption.
        if gate_environment.lab_runner.is_some()
            && error
                .details
                .get("gate_disposition")
                .and_then(Value::as_str)
                == Some("unavailable")
        {
            Ok(
                crate::agent_task_gate::placement::gate_non_execution_report(
                    index,
                    crate::agent_task_gate::legacy_gate_argv(command),
                    visibility,
                    reveal_policy,
                    &error,
                ),
            )
        } else {
            Err(gate_setup_failure(
                "destination_gate_toolchain",
                "destination_gate_workspace",
                error,
            ))
        }
    } else if let Some(supervision) = supervision.as_deref() {
        crate::agent_task_gate::run_gate_command_with_supervision(
            worktree_path,
            index,
            command,
            visibility,
            reveal_policy,
            Some(&runtime_tmpdir.context().tmp_dir),
            Some(supervision),
            &gate_environment,
            &options.gates.gate_package_artifacts,
        )
    } else {
        verify_promotion_gate(
            worktree_path,
            index,
            command,
            visibility,
            reveal_policy,
            &runtime_tmpdir.context().tmp_dir,
            &gate_environment,
            &options.gates.gate_package_artifacts,
        )
    };
    let mut result = result;
    if let Ok(report) = &mut result {
        let full_evidence_ref = format!(
            "homeboy://agent-task/run/{run_id}/promotion/gates/{}",
            report.id
        );
        crate::agent_task_gate::ingest_gate_diagnostic_sidecars(
            worktree_path,
            &options.gates.gate_diagnostic_sidecars,
            report,
            &full_evidence_ref,
        );
    }
    let evidence = match &result {
        Ok(report) => serde_json::json!({
            "gate_id": report.id,
            "status": report.status,
            "exit_code": report.exit_code,
        }),
        Err(error) => serde_json::json!({ "error": error.message }),
    };
    let reason = match &result {
        Ok(report) if report.status == AgentTaskGateStatus::Succeeded => "verification_succeeded",
        Ok(_) => "verification_failed",
        Err(_) => "verification_execution_failed",
    };
    crate::controller_scratch::release_attempt(&allocation, reason, evidence)?;
    finish_promotion_gate_run_dir(
        &run_dir,
        matches!(&result, Ok(report) if report.status == AgentTaskGateStatus::Succeeded),
    );
    result
}

fn finish_promotion_gate_run_dir(run_dir: &homeboy_core::engine::run_dir::RunDir, success: bool) {
    run_dir.finish(success);
}

fn promotion_source(
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionRequest,
) -> AgentTaskPromotionSource {
    AgentTaskPromotionSource {
        kind: source_kind.to_string(),
        task_id: outcome.task_id.clone(),
        run_id: options.source_run_id.clone(),
        path: options
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
    }
}

/// Resolve the target before materializing a candidate so a declared base can
/// be validated without mutating the working tree (#9400).
fn resolve_promotion_target_path(
    to_worktree: &str,
    materialized_workspace: Option<&Path>,
    adopted_source: Option<&Path>,
) -> Result<Option<PathBuf>> {
    // Direct paths do not need provider resolution, but must participate in
    // the same pre-apply declared-base contract as provider-owned targets.
    // An adopted revision is promoted in the source worktree that holds it,
    // where its gates run.
    let direct = Path::new(to_worktree);
    if let Some(target) = materialized_workspace
        .or_else(|| direct.is_dir().then_some(direct))
        .or_else(|| adopted_source.filter(|path| path.is_dir()))
    {
        let path = std::fs::canonicalize(target).map_err(|error| {
            Error::internal_io(error.to_string(), Some(target.display().to_string()))
        })?;
        if homeboy_core::git::output_allow_empty(&path, &["rev-parse", "--is-inside-work-tree"])
            .as_deref()
            != Some("true")
        {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                "promotion target path is not a Git worktree",
                Some(to_worktree.to_string()),
                None,
            ));
        }
        return Ok(Some(path));
    }
    match homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(to_worktree) {
        Ok(Some(target)) => Ok(target.path.is_dir().then_some(target.path)),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The base a promotion records as verified: the commit its gates actually
/// ran on, where the destination HEAD meets the declared base observed on
/// origin. A destination created before the base advanced, or a PR branch
/// carrying its own commits, sits on an older ancestor of the live tip; the
/// tip itself was never under the gates (#15017).
fn capture_gate_verified_base(
    worktree_path: &Path,
    base_ref: Option<&str>,
) -> Result<Option<AgentTaskPromotionVerifiedBase>> {
    let Some(mut observed) = capture_declared_base(worktree_path, base_ref)? else {
        return Ok(None);
    };
    observed.sha = git_output(worktree_path, &["merge-base", &observed.sha, "HEAD"])?;
    Ok(Some(observed))
}

pub(crate) fn capture_declared_base(
    worktree_path: &Path,
    base_ref: Option<&str>,
) -> Result<Option<AgentTaskPromotionVerifiedBase>> {
    capture_declared_base_with_git_and_timeout(
        worktree_path,
        base_ref,
        "git",
        &transport_environment(worktree_path),
        declared_base_git_timeout(),
    )
}

fn declared_base_git_timeout() -> Duration {
    std::env::var(DECLARED_BASE_GIT_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_DECLARED_BASE_GIT_TIMEOUT)
}

struct DeclaredBaseGitObservation {
    output: Output,
    elapsed: Duration,
}

fn capture_declared_base_with_git_and_timeout(
    worktree_path: &Path,
    base_ref: Option<&str>,
    git: &str,
    environment: &[(String, String)],
    timeout: Duration,
) -> Result<Option<AgentTaskPromotionVerifiedBase>> {
    let Some(base_ref) = base_ref.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let observed = run_declared_base_git(
        worktree_path,
        git,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{base_ref}"),
        ],
        environment,
        base_ref,
        "resolve",
        timeout,
    )?;
    if !observed.output.status.success() {
        return Err(declared_base_git_failure(
            worktree_path,
            base_ref,
            "resolve",
            "ls-remote",
            &observed.output.stderr,
            environment,
            timeout,
            observed.elapsed,
        ));
    }
    let sha = String::from_utf8_lossy(&observed.output.stdout)
        .split_whitespace()
        .next()
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "base_ref",
                format!(
                    "declared base `{base_ref}` was not found on origin before promotion gates"
                ),
                None,
                None,
            )
        })?
        .to_string();
    let fetch = homeboy_core::git::with_remote_tracking_authority_until(
        worktree_path,
        "fetch declared promotion base",
        Instant::now() + timeout,
        |remaining| {
            run_declared_base_git(
                worktree_path,
                git,
                &[
                    "fetch",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "origin",
                    &sha,
                ],
                environment,
                base_ref,
                "fetch",
                remaining,
            )
        },
    )?;
    if !fetch.output.status.success() {
        return Err(declared_base_git_failure(
            worktree_path,
            base_ref,
            "fetch",
            "fetch",
            &fetch.output.stderr,
            environment,
            timeout,
            fetch.elapsed,
        ));
    }
    let output = run_declared_base_git(
        worktree_path,
        git,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
        environment,
        base_ref,
        "verify",
        timeout,
    )?;
    if !output.output.status.success() {
        return Err(Error::validation_invalid_argument(
            "base_ref",
            format!("could not resolve declared base `{base_ref}` before promotion gates"),
            None,
            None,
        ));
    }
    Ok(Some(AgentTaskPromotionVerifiedBase {
        base: base_ref.to_string(),
        sha: String::from_utf8_lossy(&output.output.stdout)
            .trim()
            .to_string(),
    }))
}

fn transport_environment(worktree_path: &Path) -> Vec<(String, String)> {
    let mut env = homeboy_core::git::remote_origin_url(worktree_path)
        .as_deref()
        .and_then(git_remote_host)
        .map(|host| {
            homeboy_core::git::github_cli_env(
                &host,
                &homeboy_core::component::GithubConfig::default(),
            )
        })
        .unwrap_or_default();
    for key in PROXY_ENV_KEYS {
        if env
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(key))
        {
            continue;
        }
        if let Ok(value) = std::env::var(key).map(|value| value.trim().to_string()) {
            if !value.is_empty() {
                env.push(((*key).to_string(), value));
            }
        }
    }
    env
}

fn git_remote_host(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let authority = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
        .or_else(|| remote.strip_prefix("ssh://"))
        .or_else(|| remote.split_once('@').map(|(_, value)| value))?;
    let host = authority
        .split('@')
        .next_back()?
        .split('/')
        .next()?
        .split(':')
        .next()?
        .trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn origin_remote_diagnostic(worktree_path: &Path) -> Option<String> {
    homeboy_core::git::remote_origin_url(worktree_path)
        .map(|url| redact_declared_base_git_diagnostic(&url))
        .filter(|url| !url.is_empty())
}

fn proxy_env_keys(environment: &[(String, String)]) -> Vec<String> {
    environment
        .iter()
        .filter(|(key, value)| {
            !value.is_empty()
                && PROXY_ENV_KEYS
                    .iter()
                    .any(|proxy| key.eq_ignore_ascii_case(proxy))
        })
        .map(|(key, _)| key.clone())
        .collect()
}

fn phase_gerund(phase: &str) -> &'static str {
    match phase {
        "fetch" => "fetching",
        "verify" => "verifying",
        _ => "resolving",
    }
}

fn run_declared_base_git(
    worktree_path: &Path,
    git: &str,
    args: &[&str],
    environment: &[(String, String)],
    base_ref: &str,
    phase: &str,
    timeout: Duration,
) -> Result<DeclaredBaseGitObservation> {
    let subphase = args.first().copied().unwrap_or(phase);
    let started = Instant::now();
    let mut command = Command::new(git);
    command
        .args(args)
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(environment.iter().map(|(key, value)| (key, value)));
    homeboy_core::engine::command::isolate_process_tree(&mut command);
    let mut child = command.spawn().map_err(|error| {
        declared_base_transport_error(
            worktree_path,
            base_ref,
            phase,
            subphase,
            format!("could not start Git base preflight: {error}"),
            environment,
            timeout,
            started.elapsed(),
            None,
        )
    })?;
    let supervised = homeboy_core::engine::command::wait_with_bounded_output_supervised(
        &mut child,
        homeboy_core::engine::command::DEFAULT_CAPTURE_LIMIT_BYTES,
        timeout,
        DECLARED_BASE_GIT_HEARTBEAT,
        || false,
        |_, _| Ok(()),
    )
    .map_err(|error| {
        declared_base_transport_error(
            worktree_path,
            base_ref,
            phase,
            subphase,
            format!("could not supervise Git base preflight: {error}"),
            environment,
            timeout,
            started.elapsed(),
            None,
        )
    })?;
    let elapsed = started.elapsed();
    if supervised.termination
        != homeboy_core::engine::command::SupervisedCommandTermination::Completed
    {
        let stderr = redact_declared_base_git_diagnostic(&String::from_utf8_lossy(
            &supervised.output.stderr,
        ));
        let mut problem = format!(
            "Git base preflight `{subphase}` exceeded its deadline after {}ms (budget {}ms)",
            elapsed.as_millis(),
            timeout.as_millis()
        );
        if !stderr.is_empty() {
            problem = format!("{problem}: {stderr}");
        }
        return Err(declared_base_transport_error(
            worktree_path,
            base_ref,
            phase,
            subphase,
            problem,
            environment,
            timeout,
            elapsed,
            Some(stderr.as_str()).filter(|value| !value.is_empty()),
        ));
    }
    Ok(DeclaredBaseGitObservation {
        output: supervised.output.into_output(),
        elapsed,
    })
}

fn declared_base_git_failure(
    worktree_path: &Path,
    base_ref: &str,
    phase: &str,
    subphase: &str,
    stderr: &[u8],
    environment: &[(String, String)],
    timeout: Duration,
    elapsed: Duration,
) -> Error {
    let stderr = redact_declared_base_git_diagnostic(&String::from_utf8_lossy(stderr));
    if is_git_transport_failure(&stderr) {
        return declared_base_transport_error(
            worktree_path,
            base_ref,
            phase,
            subphase,
            format!("Git base preflight `{subphase}` failed: {stderr}"),
            environment,
            timeout,
            elapsed,
            Some(stderr.as_str()).filter(|value| !value.is_empty()),
        );
    }
    Error::validation_invalid_argument(
        "base_ref",
        format!("could not capture declared base `{base_ref}` before promotion gates: {stderr}"),
        None,
        None,
    )
}

fn redact_declared_base_git_diagnostic(value: &str) -> String {
    let policy = homeboy_core::redaction::RedactionPolicy::default();
    policy.redact_embedded_urls(value).trim().to_string()
}

fn declared_base_transport_error(
    worktree_path: &Path,
    base_ref: &str,
    phase: &str,
    subphase: &str,
    problem: String,
    environment: &[(String, String)],
    timeout: Duration,
    elapsed: Duration,
    stderr: Option<&str>,
) -> Error {
    let names = environment.iter().map(|(key, _)| key).collect::<Vec<_>>();
    let proxy_keys = proxy_env_keys(environment);
    let proxy_present = !proxy_keys.is_empty();
    let gerund = phase_gerund(phase);
    let mut error = Error::internal_unexpected(format!(
        "transient Git transport failure while {gerund} declared base `{base_ref}`: {problem}"
    ))
    .with_retryable(true);
    error.details["field"] = Value::String("base_ref".to_string());
    let git_stderr = stderr.unwrap_or("").to_string();
    let mut preflight = json!({
        "schema": "homeboy/git-base-preflight/v1",
        "base_ref": base_ref,
        "phase": phase,
        "subphase": subphase,
        "timeout_ms": timeout.as_millis() as u64,
        "elapsed_ms": elapsed.as_millis() as u64,
        "retry_disposition": "retryable_transport_failure",
        "required_environment": names,
        "proxy_detected": proxy_present,
        "proxy_forwarded": proxy_present,
        "git_stderr": git_stderr,
    });
    if let Some(remote) = origin_remote_diagnostic(worktree_path) {
        preflight["remote"] = Value::String(remote);
    }
    error.details["git_base_preflight"] = preflight;
    error.hints.push(homeboy_error::Hint {
        message: if proxy_present {
            "Retry Cook after restoring the controller's network/proxy environment; provider execution has not started.".to_string()
        } else {
            "Retry Cook after restoring the controller's network/proxy environment (HTTPS_PROXY/HTTP_PROXY/ALL_PROXY or github_hosts.<host>.proxy); provider execution has not started.".to_string()
        },
    });
    error
}

/// Classify a base-preflight failure as transport rather than a bad ref.
///
/// Delegates to the shared core classifier (`classify_git_push_failure`) so
/// promotion's base preflight and finalization's publication push report one
/// consistent transport taxonomy: every typed outcome except `Other` is
/// transport evidence. Every needle names diagnostic evidence emitted by Git,
/// SSH, or the TLS stack; a remote host that merely contains the word
/// `proxy` is not evidence.
fn is_git_transport_failure(stderr: &str) -> bool {
    homeboy_core::git::classify_git_push_failure(stderr) != GitPushTransportClass::Other
}

fn status_for_report(
    dry_run: bool,
    has_gate_failure: bool,
    has_gate_deferral: bool,
) -> AgentTaskPromotionStatus {
    if dry_run {
        AgentTaskPromotionStatus::DryRun
    } else if has_gate_failure {
        // A failure is decisive evidence even alongside a deferral elsewhere:
        // the candidate is rejected regardless of what else could not run.
        AgentTaskPromotionStatus::GateFailed
    } else if has_gate_deferral {
        AgentTaskPromotionStatus::GateDeferred
    } else {
        AgentTaskPromotionStatus::Applied
    }
}

fn promotion_notification(
    status: AgentTaskPromotionStatus,
    target: &AgentTaskPromotionTarget,
) -> AgentTaskPromotionNotification {
    let target_path = target.path.as_deref().unwrap_or(target.worktree.as_str());
    match status {
        AgentTaskPromotionStatus::VerificationPending => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted; deterministic verification is pending".to_string(),
            resumable_blocker: Some("rerun promotion to resume verification without reapplying the patch".to_string()),
            next_command: None,
        },
        AgentTaskPromotionStatus::Applied => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: format!(
                "patch promoted into {}; finalize from {} using the verified_base_sha recorded in this promotion report",
                target.worktree, target_path
            ),
            resumable_blocker: None,
            next_command: None,
        },
        AgentTaskPromotionStatus::GateFailed => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted, but deterministic gates failed".to_string(),
            resumable_blocker: Some(
                "run `homeboy agent-task gate-feedback` with the promotion report, then retry the follow-up request".to_string(),
            ),
            next_command: None,
        },
        AgentTaskPromotionStatus::GateDeferred => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "patch promoted, but at least one deterministic gate could not execute under the resolved placement and deferred; the candidate is unverified, not rejected".to_string(),
            resumable_blocker: Some(
                "retry verification once the required environment (e.g. a ready Lab runner) is available; the existing candidate does not need to be regenerated".to_string(),
            ),
            next_command: None,
        },
        AgentTaskPromotionStatus::VerifiedNoChanges => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "provider produced no patch; the pinned candidate workspace passed deterministic verification".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
        AgentTaskPromotionStatus::NoChangesGateFailed => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "provider produced no patch, but deterministic verification failed in the pinned candidate workspace".to_string(),
            resumable_blocker: Some("repair the candidate and rerun Cook so the declared verification gates pass".to_string()),
            next_command: None,
        },
        AgentTaskPromotionStatus::DryRun => AgentTaskPromotionNotification {
            status: "blocked".to_string(),
            message: "dry run validated a patch artifact but did not apply it".to_string(),
            resumable_blocker: Some("rerun promote without `--dry-run` to apply the patch".to_string()),
            next_command: Some(format!(
                "homeboy agent-task promote <run-id> --to-worktree {}",
                target.worktree
            )),
        },
        AgentTaskPromotionStatus::NoChanges => AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "provider completed successfully but produced an empty patch; nothing was promoted".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
    }
}

fn promotion_notification_with_gate_summary(
    status: AgentTaskPromotionStatus,
    target: &AgentTaskPromotionTarget,
    gates: &[crate::agent_task_gate::AgentTaskGateReport],
) -> AgentTaskPromotionNotification {
    let mut notification = promotion_notification(status, target);
    if !gates.is_empty() {
        let gate_ids = |statuses: &[AgentTaskGateStatus]| {
            gates
                .iter()
                .filter(|gate| statuses.contains(&gate.status))
                .map(|gate| gate.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let passed = gate_ids(&[AgentTaskGateStatus::Succeeded]);
        let failed = gate_ids(&[
            AgentTaskGateStatus::Failed,
            AgentTaskGateStatus::AcceptedInheritedFailure,
        ]);
        let skipped = gate_ids(&[AgentTaskGateStatus::Skipped]);
        let deferred = gate_ids(&[AgentTaskGateStatus::Deferred]);
        notification.message.push_str(&format!(
            "; deterministic gates: passed=[{passed}], failed=[{failed}], skipped=[{skipped}], deferred=[{deferred}]"
        ));
    }
    notification
}

#[expect(
    clippy::too_many_arguments,
    reason = "report construction keeps durable promotion evidence explicit"
)]
fn post_apply_report(
    options: &AgentTaskPromotionRequest,
    source_kind: &str,
    outcome: &AgentTaskOutcome,
    patch_artifact: AgentTaskPromotionArtifactRef,
    changed_files: Vec<String>,
    command_evidence: Vec<AgentTaskPromotionCommandReport>,
    target: AgentTaskPromotionTarget,
    worktree_path: &Path,
    source_schema: &str,
    artifact_metadata: Value,
    verified_base: Option<AgentTaskPromotionVerifiedBase>,
) -> Result<AgentTaskPromotionReport> {
    let status = AgentTaskPromotionStatus::VerificationPending;
    let operator_notification = promotion_notification(status, &target);
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(&worktree_path.display().to_string())
            .ok();
    let gate_feedback_baseline = match candidate.as_ref() {
        Some(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { .. }) => Some(json!({
            "schema": "homeboy/agent-task-gate-feedback-baseline/v1",
            "current_diff": candidate_current_diff(worktree_path)?,
        })),
        _ => None,
    };
    // Managed targets pass their pre-apply snapshot. Unmanaged targets may not
    // expose a path until apply, so retain the established best-effort read.
    let verified_base = verified_base.or_else(|| {
        capture_gate_verified_base(worktree_path, options.base_ref.as_deref())
            .ok()
            .flatten()
    });
    Ok(AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status,
        source: promotion_source(source_kind, outcome, options),
        to_worktree: options.to_worktree.clone(),
        target: target.clone(),
        patch_artifact: patch_artifact.clone(),
        changed_files,
        command_evidence,
        deterministic_gates: Vec::new(),
        gate_results: Vec::new(),
        verified_base: verified_base.clone(),
        repository_integrity_evidence: options.repository_integrity_evidence.clone(),
        provenance: json!({
            "source_schema": source_schema,
            "artifact_metadata": artifact_metadata,
            "worktree_path": worktree_path,
            "dependencies_materialized": false,
            "post_apply": true,
            "candidate": candidate,
            "destination_baseline": candidate,
            "gate_feedback_baseline": gate_feedback_baseline,
            "resume_inputs": {
                "base_ref": options.base_ref,
                "task_base_sha": options.task_base_sha,
                "candidate_ref": options.candidate_ref,
            },
            "resume_contract": {
                "resolved_base": verified_base,
                "artifact": patch_artifact,
                "destination": target,
                "candidate": candidate,
                "inputs": {
                    "base_ref": options.base_ref,
                    "task_base_sha": options.task_base_sha,
                    "candidate_ref": options.candidate_ref,
                },
                "gates": options.gates,
            },
        }),
        operator_notification,
    })
}

/// Capture the complete candidate delta without changing the destination index.
fn candidate_current_diff(worktree_path: &Path) -> Result<String> {
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create gate-feedback candidate index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    for arguments in [vec!["read-tree", "HEAD"], vec!["add", "--all"]] {
        let output = Command::new("git")
            .args(arguments)
            .env("GIT_INDEX_FILE", &index_path)
            .current_dir(worktree_path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
    }
    let output = Command::new("git")
        .args(["diff", "--cached", "--binary", "--full-index", "HEAD"])
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(worktree_path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn persisted_changed_files(
    patch_changed_files: Vec<String>,
    candidate: Option<&crate::agent_task_promotion::AgentTaskPromotionCandidate>,
) -> Vec<String> {
    match candidate {
        Some(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint })
            if !fingerprint.changed_files.is_empty() =>
        {
            fingerprint.changed_files.clone()
        }
        _ => patch_changed_files,
    }
}

fn select_outcome(source: Value, task_id: Option<&str>) -> Result<(String, AgentTaskOutcome)> {
    if source.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_OUTCOME_SCHEMA) {
        let outcome: AgentTaskOutcome = serde_json::from_value(source).map_err(|error| {
            Error::validation_invalid_json(error, Some("agent-task outcome".to_string()), None)
        })?;
        if let Some(expected) = task_id {
            if outcome.task_id != expected {
                return Err(Error::validation_invalid_argument(
                    "task_id",
                    format!(
                        "source outcome task_id is {}, not {expected}",
                        outcome.task_id
                    ),
                    None,
                    None,
                ));
            }
        }
        return Ok(("outcome".to_string(), outcome));
    }

    if source.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_AGGREGATE_SCHEMA) {
        let aggregate: AgentTaskAggregate = serde_json::from_value(source).map_err(|error| {
            Error::validation_invalid_json(error, Some("agent-task aggregate".to_string()), None)
        })?;
        let candidates: Vec<AgentTaskOutcome> = aggregate
            .outcomes
            .into_iter()
            .filter(|outcome| task_id.is_none_or(|expected| outcome.task_id == expected))
            .collect();
        return match candidates.len() {
            1 => Ok((
                "aggregate".to_string(),
                candidates.into_iter().next().unwrap(),
            )),
            0 => Err(Error::validation_invalid_argument(
                "task_id",
                "aggregate did not contain a matching outcome",
                None,
                None,
            )),
            _ => Err(Error::validation_invalid_argument(
                "task_id",
                "aggregate contains multiple outcomes; pass --task-id to select one",
                None,
                None,
            )),
        };
    }

    Err(Error::validation_invalid_argument(
        "source",
        "promotion source must be an agent-task outcome or aggregate JSON object",
        None,
        None,
    ))
}

pub(crate) fn select_patch_artifact(
    outcome: &AgentTaskOutcome,
    artifact_id: Option<&str>,
) -> Result<AgentTaskArtifact> {
    let artifacts: Vec<AgentTaskArtifact> = outcome
        .artifacts
        .iter()
        .filter(|artifact| artifact_id.is_none_or(|expected| artifact.id == expected))
        .filter(|artifact| {
            is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact)
        })
        .cloned()
        .collect();

    let actionable_artifacts: Vec<AgentTaskArtifact> = artifacts
        .iter()
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect();
    if !actionable_artifacts.is_empty() {
        return match actionable_artifacts.len() {
            1 => Ok(actionable_artifacts.into_iter().next().unwrap()),
            _ => Err(Error::validation_invalid_argument(
                "artifact_id",
                "multiple patch artifacts were found; pass --artifact-id to select one",
                None,
                None,
            )),
        };
    }

    match artifacts.len() {
        1 => Ok(artifacts.into_iter().next().unwrap()),
        0 => Err(Error::validation_invalid_argument(
            "artifact_id",
            "no matching patch artifact was found; inspect the agent result or transcript for diagnosis",
            None,
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "artifact_id",
            "multiple patch artifacts were found; pass --artifact-id to select one",
            None,
            None,
        )),
    }
}

const CANONICAL_PATCH_BYTE_BUDGET_EXCEEDED: &str = "canonical_patch_byte_budget_exceeded";

#[derive(Debug, Clone)]
pub struct CanonicalRecoverablePatchArtifacts {
    pub artifacts: Vec<AgentTaskArtifact>,
    /// Normalized patch bodies keyed by canonical artifact ID. Callers use this
    /// only for bounded, deterministic projections; full patches remain at their
    /// durable artifact addresses. Candidates compared by stored SHA-256 and
    /// size because they exceed the comparison byte budget have no entry.
    pub patch_contents: BTreeMap<String, String>,
    pub unavailable: Vec<Value>,
    pub omitted_candidate_count: usize,
    pub omitted_patch_bytes: u64,
}

/// Resolve and normalize recoverable provider artifacts into deterministic patch
/// choices. Both review and promotion use this contract after lifecycle
/// materialization, including controller projections and hydrated runner bytes.
pub fn canonical_recoverable_patch_artifacts(
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionRequest,
) -> Result<CanonicalRecoverablePatchArtifacts> {
    // One call is one unit of work, so the store resolves once here rather
    // than at each projection lookup inside (#7505).
    let observation_store = homeboy_core::observation::ObservationStore::open_initialized()?;
    canonical_recoverable_patch_artifacts_internal(outcome, options, &observation_store)
}

pub(crate) fn canonical_recoverable_patch_artifacts_in_observation_store(
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionRequest,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<CanonicalRecoverablePatchArtifacts> {
    canonical_recoverable_patch_artifacts_internal(outcome, options, observation_store)
}

fn canonical_recoverable_patch_artifacts_internal(
    outcome: &AgentTaskOutcome,
    options: &AgentTaskPromotionRequest,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<CanonicalRecoverablePatchArtifacts> {
    let mut candidates = outcome
        .artifacts
        .iter()
        .filter(|artifact| {
            options
                .artifact_id
                .as_deref()
                .is_none_or(|id| artifact.id == id)
        })
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    let omitted_candidate_count = candidates
        .len()
        .saturating_sub(CANONICAL_PATCH_CANDIDATE_LIMIT);
    candidates.truncate(CANONICAL_PATCH_CANDIDATE_LIMIT);

    // A sole candidate (including one chosen explicitly by artifact ID) needs
    // no comparison, so the comparison byte budget does not apply to it.
    let sole_candidate = candidates.len() == 1;
    let mut canonical = Vec::<CanonicalCandidate>::new();
    let mut unavailable = Vec::new();
    let mut read_patch_bytes = 0_u64;
    let mut omitted_patch_bytes = 0_u64;
    for mut artifact in candidates {
        let Some(kind) = canonical_patch_kind(&artifact.kind) else {
            continue;
        };
        artifact.kind = kind.to_string();
        let path = match resolve_artifact_path(
            &artifact,
            &outcome.task_id,
            options.source_run_id.as_deref(),
            options.source_path.as_deref(),
            observation_store,
        ) {
            Ok(path) => path,
            Err(error) => {
                unavailable.push(json!({ "id": artifact.id, "reason": error.message }));
                continue;
            }
        };
        let patch_bytes = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                unavailable
                    .push(json!({ "id": artifact.id, "path": path, "reason": error.to_string() }));
                continue;
            }
        };
        if !sole_candidate
            && read_patch_bytes.saturating_add(patch_bytes) > CANONICAL_PATCH_BYTES_TOTAL_LIMIT
        {
            // Compare by reference: the stored SHA-256 and size identify the
            // patch without loading its bytes into memory. The digest is
            // verified by streaming the artifact file.
            let Some(stored_sha256) = artifact
                .sha256
                .clone()
                .filter(|sha256| valid_sha256(sha256))
            else {
                omitted_patch_bytes = omitted_patch_bytes.saturating_add(patch_bytes);
                unavailable.push(json!({
                    "id": artifact.id,
                    "reason": CANONICAL_PATCH_BYTE_BUDGET_EXCEEDED,
                    "size_bytes": patch_bytes,
                    "limit_bytes": CANONICAL_PATCH_BYTES_TOTAL_LIMIT,
                }));
                continue;
            };
            if let Some(expected_size) = artifact.size_bytes {
                if expected_size != patch_bytes {
                    unavailable.push(json!({
                        "id": artifact.id,
                        "path": path,
                        "reason": format!(
                            "patch artifact size mismatch: expected {expected_size} bytes, read {patch_bytes} bytes"
                        ),
                    }));
                    continue;
                }
            }
            match content_hash::sha256_file(&path) {
                Ok(actual) if actual == stored_sha256 => {}
                Ok(actual) => {
                    unavailable.push(json!({
                        "id": artifact.id,
                        "path": path,
                        "reason": format!(
                            "patch artifact sha256 mismatch: expected {stored_sha256}, read {actual}"
                        ),
                    }));
                    continue;
                }
                Err(error) => {
                    unavailable
                        .push(json!({ "id": artifact.id, "path": path, "reason": error.message }));
                    continue;
                }
            }
            omitted_patch_bytes = omitted_patch_bytes.saturating_add(patch_bytes);
            let stored_identity =
                canonical_patch_stored_identity(&artifact, &stored_sha256, patch_bytes);
            if !canonical
                .iter()
                .any(|existing| existing.stored_identity == stored_identity)
            {
                canonical.push(CanonicalCandidate {
                    normalized_identity: None,
                    stored_identity,
                    artifact,
                    content: None,
                });
            }
            continue;
        }
        let patch = match std::fs::read_to_string(&path) {
            Ok(patch) => patch,
            Err(error) => {
                unavailable
                    .push(json!({ "id": artifact.id, "path": path, "reason": error.to_string() }));
                continue;
            }
        };
        read_patch_bytes = read_patch_bytes.saturating_add(patch_bytes);
        if let Err(error) = validate_artifact_content(&artifact, &patch) {
            unavailable.push(json!({ "id": artifact.id, "path": path, "reason": error.message }));
            continue;
        }
        let normalized = match normalize_promotion_patch(&patch, &options.to_worktree) {
            Ok(patch) => patch,
            Err(error) => {
                unavailable
                    .push(json!({ "id": artifact.id, "path": path, "reason": error.message }));
                continue;
            }
        };
        let stored_identity = canonical_patch_stored_identity(
            &artifact,
            &content_hash::sha256_hex(patch.as_bytes()),
            patch_bytes,
        );
        drop(patch);
        let digest = content_hash::sha256_hex(normalized.content.as_bytes());
        let identity = canonical_patch_identity_with_digest(&artifact, &digest);
        if !canonical.iter().any(|existing| {
            existing.normalized_identity.as_deref() == Some(identity.as_str())
                || existing.stored_identity == stored_identity
        }) {
            canonical.push(CanonicalCandidate {
                normalized_identity: Some(identity),
                stored_identity,
                artifact,
                content: Some(normalized.content),
            });
        }
    }

    Ok(CanonicalRecoverablePatchArtifacts {
        artifacts: canonical
            .iter()
            .map(|candidate| candidate.artifact.clone())
            .collect(),
        patch_contents: canonical
            .into_iter()
            .filter_map(|candidate| {
                candidate
                    .content
                    .map(|content| (candidate.artifact.id, content))
            })
            .collect(),
        unavailable,
        omitted_candidate_count,
        omitted_patch_bytes,
    })
}

struct CanonicalCandidate {
    /// Digest of the normalized patch body, when the bytes were read.
    normalized_identity: Option<String>,
    /// Stored SHA-256 and size of the raw artifact bytes.
    stored_identity: String,
    artifact: AgentTaskArtifact,
    content: Option<String>,
}

fn canonical_patch_stored_identity(
    artifact: &AgentTaskArtifact,
    sha256: &str,
    size: u64,
) -> String {
    format!("{sha256}|{size}|{}", canonical_patch_provenance(artifact))
}

fn canonical_patch_identity_with_digest(artifact: &AgentTaskArtifact, digest: &str) -> String {
    format!("{digest}|{}", canonical_patch_provenance(artifact))
}

fn canonical_patch_provenance(artifact: &AgentTaskArtifact) -> String {
    [
        "run_id",
        "task_id",
        "producer_attempt",
        "base_ref",
        "provider_backend",
        "repository_identity",
        "workspace_identity",
    ]
    .iter()
    .map(|key| {
        artifact
            .metadata
            .get(*key)
            .cloned()
            .unwrap_or(Value::Null)
            .to_string()
    })
    .collect::<Vec<_>>()
    .join("|")
}

fn canonical_patch_kind(kind: &str) -> Option<&'static str> {
    is_patch_artifact_kind(kind).then_some("patch")
}

fn resolve_artifact_path(
    artifact: &AgentTaskArtifact,
    task_id: &str,
    source_run_id: Option<&str>,
    source_path: Option<&Path>,
    observation_store: &homeboy_core::observation::ObservationStore,
) -> Result<PathBuf> {
    if let Some(run_id) = source_run_id {
        let projected =
            crate::agent_task_lifecycle::verified_controller_artifact_projection_path_in_store(
                observation_store,
                run_id,
                task_id,
                artifact,
            )?;
        if let Some(projected) = projected {
            return Ok(projected);
        }
    }
    let path = artifact.path.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact.path",
            "promotion patch artifact must provide a local path or a verified controller-side artifact projection",
            None,
            None,
        )
    })?;
    let path = PathBuf::from(path);
    if path.is_absolute() {
        if !path.is_file() {
            return Err(Error::validation_invalid_argument(
                "artifact.path",
                "promotion could not find a verified controller-side artifact projection; reconcile the run on this controller before promoting its runner-produced artifact",
                Some(path.display().to_string()),
                None,
            ));
        }
        return Ok(path);
    }
    if let Some(source_path) = source_path.and_then(Path::parent) {
        Ok(source_path.join(path))
    } else {
        Ok(path)
    }
}

fn validate_workspace_handle(handle: &str) -> Result<()> {
    if handle.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "target workspace handle must not be empty",
            None,
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod gate_setup_message_tests {
    use super::*;
    use homeboy_core::deps::{
        DependencyHydrationOutcome, DependencyHydrationStatus, DependencyHydrationTermination,
    };

    fn outcome(
        termination: DependencyHydrationTermination,
        exit_code: Option<i32>,
        stderr: &str,
    ) -> DependencyHydrationOutcome {
        DependencyHydrationOutcome {
            schema: "homeboy/dependency-hydration-outcome/v1".to_string(),
            workspace: "destination_gate_workspace".to_string(),
            package_root: ".".to_string(),
            provider_id: "pnpm".to_string(),
            command: vec![
                "pnpm".to_string(),
                "install".to_string(),
                "--frozen-lockfile".to_string(),
            ],
            cwd: String::new(),
            reason: "lockfile present".to_string(),
            duration_ms: 12,
            termination,
            status: DependencyHydrationStatus::Failed,
            exit_code,
            stdout: String::new(),
            stderr: stderr.to_string(),
            missing_outputs: Vec::new(),
        }
    }

    /// #15338: a missing package manager must read as a missing tool, not as
    /// an unknown Homeboy component named after the provider.
    #[test]
    fn missing_package_manager_names_the_tool_to_install() {
        let error = gate_setup_outcome_failure(
            "destination_gate_setup",
            &outcome(
                DependencyHydrationTermination::SpawnFailed,
                None,
                "No such file or directory (os error 2)",
            ),
        );
        assert!(error
            .message
            .starts_with("Dependency step 'promotion.gate_setup' failed for component 'pnpm': "));
        assert!(error
            .message
            .contains("`pnpm install --frozen-lockfile` could not be started"));
        assert!(error.message.contains("install `pnpm`"));
        assert!(error.message.contains("stderr: No such file or directory"));
        // Structured details are unchanged.
        assert_eq!(error.details["component_id"], "pnpm");
    }

    #[test]
    fn stalled_dependency_diagnostic_includes_command_cwd_and_last_output() {
        let mut stalled = outcome(DependencyHydrationTermination::NoProgress, None, "");
        stalled.cwd = "/workspace/packages/api".to_string();
        stalled.stdout = "installing packages\nwaiting for input".to_string();
        let error = gate_setup_outcome_failure("destination_gate_setup", &stalled);
        assert!(error.message.contains("`pnpm install --frozen-lockfile`"));
        assert!(error.message.contains("cwd: /workspace/packages/api"));
        assert!(error
            .message
            .contains("last output: stdout: waiting for input"));
    }

    #[test]
    fn explicit_component_gate_workspace_is_preserved_for_remote_execution() {
        let options = AgentTaskPromotionRequest {
            source: String::new(),
            source_run_id: None,
            source_path: None,
            source_worktree_path: None,
            to_worktree: String::new(),
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: Default::default(),
            provider_invocation: None,
            repository_integrity_evidence: None,
        };
        let candidate = Path::new("/workspace");
        let component = Path::new("/workspace/packages/api");
        assert_eq!(
            promotion_gate_workspace(&options, candidate, Some(component)),
            component
        );
    }

    #[test]
    fn failed_install_reports_exit_status_and_first_stderr_line() {
        let error = gate_setup_outcome_failure(
            "destination_gate_setup",
            &outcome(
                DependencyHydrationTermination::ExitFailure,
                Some(1),
                "\n ERR_PNPM_OUTDATED_LOCKFILE  Cannot install with frozen-lockfile\nmore detail",
            ),
        );
        assert!(error.message.contains("exited with status 1"));
        let noisy = gate_setup_outcome_failure(
            "destination_gate_setup",
            &outcome(
                DependencyHydrationTermination::ExitFailure,
                Some(1),
                "(node:1) [DEP0169] DeprecationWarning: `url.parse()` behavior is not standardized\n(Use `node --trace-deprecation ...` to show where the warning was created)\nScope: all 5 workspace projects\n ERR_PNPM_LOCKFILE_CONFIG_MISMATCH  Cannot proceed with the frozen installation.\n\nUpdate your lockfile",
            ),
        );
        assert!(
            noisy
                .message
                .ends_with("stderr: ERR_PNPM_LOCKFILE_CONFIG_MISMATCH  Cannot proceed with the frozen installation."),
            "{}",
            noisy.message
        );
        assert!(error
            .message
            .ends_with("stderr: ERR_PNPM_OUTDATED_LOCKFILE  Cannot install with frozen-lockfile"));
    }
}

#[cfg(test)]
mod gate_setup_failure_retry_tests {
    use super::*;

    #[test]
    fn non_retryable_validation_cause_is_a_configuration_fix_not_a_hydration_retry() {
        let mut cause = Error::validation_invalid_argument(
            "secret_env",
            "missing runner secret env ref for WP_CODEBOX_DB_HOST",
            None,
            None,
        );
        cause.retryable = Some(false);

        let failure = gate_setup_failure(
            "destination_gate_toolchain",
            "destination_gate_workspace",
            cause,
        );

        assert_eq!(failure.retryable, Some(false));
        assert_eq!(
            failure.details["cause"]["retry_action"],
            GATE_SETUP_FIX_CONFIGURATION
        );
        assert!(failure.details["cause"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("WP_CODEBOX_DB_HOST")));
    }

    #[test]
    fn environmental_causes_keep_the_hydration_retry() {
        let cause = Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "ssh: connection reset",
            serde_json::json!({}),
        );

        let failure = gate_setup_failure(
            "destination_gate_toolchain",
            "destination_gate_workspace",
            cause,
        );

        assert_ne!(failure.retryable, Some(false));
        assert_eq!(
            failure.details["cause"]["retry_action"],
            "retry_dependency_hydration"
        );
    }
}
