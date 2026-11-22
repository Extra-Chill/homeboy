//! Agent-task cook follow-up baseline materialization.
//!
//! Extracted from `cook.rs`: the process-local machinery that materializes the
//! git baseline a cook retry runs against. `CookFollowUpBaseline` owns a
//! detached `git worktree` (cleaned up in its `Drop`), `DerivedCookBaselineCapability`
//! is the non-serializable controller-validated authority derived from it, and
//! `materialize_initial_candidate_baseline`/`materialize_follow_up_baseline_in_root`
//! build them from a source root or a prior promotion. The private `git_output`
//! helpers live here because this cluster is their only user.

use crate::agent_task_gate::{AgentTaskGatePackageArtifactProvenance, AgentTaskGateReport};
use homeboy_core::observation::store::{
    coordination::{ExpectedRevision, RecordWrite},
    ObservationStore,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

use crate::agent_task_gate::{
    failure_fingerprint, run_declared_test_with_timeout, run_gate_command_with_timeout,
    AgentTaskGateBaselineComparison, AgentTaskGateDifferentialResult,
    AgentTaskGateFailureAttribution, AgentTaskGateFailureOrigin, AgentTaskGateInvocation,
    AgentTaskGateStatus,
};
use crate::agent_task_promotion::AgentTaskPromotionReport;
use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

/// A cook-owned detached checkout turns the already-promoted dirty candidate
/// into a clean commit before the scheduler creates its normal attempt checkout.
pub(crate) struct CookFollowUpBaseline {
    source_root: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) capability: DerivedCookBaselineCapability,
}

pub(crate) fn cook_attempt_harvest_context(
    harvest_context: &crate::agent_task_scheduler::HarvestExecutionContext,
) -> crate::agent_task_scheduler::HarvestExecutionContext {
    harvest_context.clone()
}

/// Process-local authority for one materialized cook retry baseline. It is not
/// serializable and never enters a request, environment, or durable record.
pub struct DerivedCookBaselineCapability {
    canonical_path: PathBuf,
    commit: String,
    tree: String,
    source_run_id: String,
    source_task_id: String,
    bound_task_id: String,
    parent_snapshot: Option<Value>,
    preexisting_candidate: bool,
    baseline_admission: Option<Value>,
}

impl DerivedCookBaselineCapability {
    pub fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }

    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }

    pub(crate) fn tree(&self) -> &str {
        &self.tree
    }

    pub(crate) fn bound_task_id(&self) -> &str {
        &self.bound_task_id
    }

    pub(crate) fn parent_snapshot(&self) -> Option<&Value> {
        self.parent_snapshot.as_ref()
    }

    /// Evidence derived from the controller-validated capability. It is not
    /// authorization for remote workspace or snapshot verification.
    pub fn verified_baseline_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "bound_task_id": self.bound_task_id,
            "baseline_commit": self.commit,
            "baseline_tree": self.tree,
            "parent_snapshot_identity": self.parent_snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .get("workspace_snapshot_identity")
                    .cloned()
                    .or_else(|| snapshot.get("identity").cloned())
            }),
            "preexisting_candidate": self.preexisting_candidate,
            "baseline_admission": self.baseline_admission,
        })
    }
}

impl CookFollowUpBaseline {
    pub(crate) fn capability(&self) -> &DerivedCookBaselineCapability {
        &self.capability
    }

    /// A lifecycle retry persists this controller-materialized checkout for a
    /// later provider dispatch. Unlike the in-process Cook path, no owner is
    /// available to remove it when this function returns.
    pub(crate) fn preserve_for_retry(self) {
        std::mem::forget(self);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_derived_cook_baseline_capability(
    path: PathBuf,
    commit: String,
    tree: String,
    task_id: &str,
    parent_snapshot: Option<Value>,
) -> DerivedCookBaselineCapability {
    DerivedCookBaselineCapability {
        canonical_path: path
            .canonicalize()
            .expect("test baseline path canonicalizes"),
        commit,
        tree,
        source_run_id: "test-source-run".to_string(),
        source_task_id: task_id.to_string(),
        bound_task_id: task_id.to_string(),
        parent_snapshot,
        preexisting_candidate: false,
        baseline_admission: None,
    }
}

/// Materialize a Cook-declared dirty candidate in a detached checkout before
/// provider dispatch. The caller workspace is never staged, reset, or edited.
pub(crate) fn materialize_initial_candidate_baseline(
    plan: &AgentTaskPlan,
    source_root: Option<&std::path::Path>,
    source_run_id: &str,
) -> Result<Option<CookFollowUpBaseline>> {
    let Some(source_root) = source_root else {
        return Ok(None);
    };
    let status = git_output(
        source_root,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        return Ok(None);
    }
    let task_id = plan
        .tasks
        .first()
        .map(|task| task.task_id.as_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "plan.tasks",
                "Cook cannot adopt a dirty candidate without a provider task",
                None,
                None,
            )
        })?;
    if plan.tasks.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "plan.tasks",
            "Cook can adopt a pre-existing candidate only for a single provider task",
            None,
            Some(vec![
                "Run one Cook task per dirty candidate workspace.".to_string()
            ]),
        ));
    }
    let gate_feedback =
        plan.tasks[0].metadata["cook_loop"]["kind"] == "deterministic-gate-feedback";
    if gate_feedback {
        crate::agent_task_candidate_tree::validate_gate_feedback_candidate_baseline(
            source_root,
            &plan.tasks[0].inputs["cook_loop"],
        )?;
    }
    let base = git_output(source_root, &["rev-parse", "HEAD"])?;
    let index = homeboy_core::git::private_index(source_root)?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        source_root,
        &["read-tree", &base],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let tree = git_output_with_env(
        source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let commit = git_output_with_env(
        source_root,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "homeboy: Cook pre-existing candidate baseline",
        ],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    // A Cook baseline is a full `git worktree` checkout of the candidate tree.
    // Placing it in `std::env::temp_dir()` put a whole working tree outside
    // every cleanup surface -- no owner record, no pin, no run binding, and
    // invisible to the retained-storage report -- and broke outright on a
    // `noexec` `/tmp` (#11128). The artifact root is the volume the operator
    // already sized for run bytes.
    let parent = homeboy_core::artifacts::root()?.join("cook-baseline");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook candidate baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        source_root,
        &["worktree", "add", "--detach", &path_string, &commit],
    )?;
    let canonical_path = path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook candidate baseline".to_string()),
        )
    })?;
    Ok(Some(CookFollowUpBaseline {
        source_root: source_root.to_path_buf(),
        path,
        capability: DerivedCookBaselineCapability {
            canonical_path,
            commit,
            tree,
            source_run_id: source_run_id.to_string(),
            source_task_id: task_id.to_string(),
            bound_task_id: task_id.to_string(),
            parent_snapshot: None,
            // A gate retry adopts an authenticated failed candidate, not an
            // initial user-supplied dirty workspace. Lab uses this distinction
            // to admit the clean derived checkout for the bound retry task.
            preexisting_candidate: !gate_feedback,
            baseline_admission: None,
        },
    }))
}

impl Drop for CookFollowUpBaseline {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.source_root)
            .status();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_root)
            .status();
    }
}

pub(crate) fn materialize_follow_up_baseline_in_root(
    promotion: &AgentTaskPromotionReport,
    artifact_root: &std::path::Path,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    materialize_follow_up_baseline_at(
        promotion,
        Some(artifact_root),
        None,
        source_run_id,
        bound_task_id,
    )
}

/// Replay unresolved failed gates against the immutable verified base. This is
/// controller work, not provider remediation: callers persist the resulting
/// promotion before evaluating Cook feedback.
fn replay_gate_policy(
    environment: &crate::agent_task_gate::AgentTaskGateEnvironment,
    component_id: Option<&str>,
) -> crate::agent_task_gate::AgentTaskGateEnvironmentPolicy {
    let mut policy = environment.replay_policy();
    policy.admitted_component_id = component_id.map(str::to_string);
    policy
}

const BASELINE_RECORDS: &str = "cook-gate-baseline-v1";

#[derive(Serialize, Deserialize)]
struct BaselineFacts {
    exit_code: i32,
    status: AgentTaskGateStatus,
    failure_fingerprint: String,
    #[serde(default)]
    failure_records: Vec<String>,
    /// Retained for compatibility with previously recorded baseline facts.
    #[serde(default)]
    diagnostic_fingerprint: Option<String>,
    package_artifacts: Vec<AgentTaskGatePackageArtifactProvenance>,
    #[serde(default)]
    output_tail: String,
}

impl BaselineFacts {
    fn from_gate(gate: &AgentTaskGateReport) -> Self {
        Self {
            exit_code: gate.exit_code,
            status: gate.status,
            failure_fingerprint: failure_fingerprint(
                gate.exit_code,
                &gate.stdout,
                &gate.stderr,
                gate.failure_evidence
                    .as_ref()
                    .map(|e| e.diagnostics.as_slice())
                    .unwrap_or_default(),
            ),
            failure_records: {
                let mut records =
                    crate::agent_task_gate::failure_records(&gate.stdout, &gate.stderr);
                if let Some(evidence) = &gate.failure_evidence {
                    // Output parsing already records libtest failures as
                    // `test:<id>`; structured evidence for the same test must
                    // not count it twice.
                    let structured = evidence
                        .failed_tests
                        .iter()
                        .filter(|test| !records.contains(&format!("test:{}", test.id)))
                        .map(|test| format!("{test:?}"))
                        .collect::<Vec<_>>();
                    records.extend(structured);
                }
                records.sort();
                records.dedup();
                records
            },
            diagnostic_fingerprint: Some(failure_fingerprint(
                gate.exit_code,
                "",
                "",
                gate.failure_evidence
                    .as_ref()
                    .map(|e| e.diagnostics.as_slice())
                    .unwrap_or_default(),
            )),
            package_artifacts: gate.environment.package_artifacts.clone(),
            output_tail: format!("{}\n{}", gate.stdout, gate.stderr)
                .chars()
                .rev()
                .take(4096)
                .collect::<String>()
                .chars()
                .rev()
                .collect(),
        }
    }

    fn comparable(&self) -> bool {
        self.exit_code != 124
            && matches!(
                self.status,
                AgentTaskGateStatus::Succeeded | AgentTaskGateStatus::Failed
            )
    }

    fn apply(&self, gate: &mut AgentTaskGateReport, base_sha: &str, diagnostic: Option<String>) {
        self.apply_candidate(
            gate,
            base_sha,
            diagnostic,
            &Self::from_gate(gate),
            Vec::new(),
            false,
        );
    }

    fn apply_candidate(
        &self,
        gate: &mut AgentTaskGateReport,
        base_sha: &str,
        diagnostic: Option<String>,
        candidate: &Self,
        flaky: Vec<String>,
        reproduced_named: bool,
    ) {
        let inherited: Vec<_> = candidate
            .failure_records
            .iter()
            .filter(|record| self.failure_records.contains(record))
            .cloned()
            .collect();
        let new: Vec<_> = candidate
            .failure_records
            .iter()
            .filter(|record| !self.failure_records.contains(record))
            .cloned()
            .collect();
        // Opaque gates have only an exit-status contract. Do not attribute an
        // already-red host or toolchain to the candidate based on output text.
        // That excuse applies only when neither run named its failures: a
        // candidate that names failures the baseline cannot account for (or
        // a baseline that names failures the candidate replaced with opaque
        // output) is not a reproduction of the baseline breakage.
        let opaque = self.failure_records.is_empty() && candidate.failure_records.is_empty();
        let comparable_records =
            !self.failure_records.is_empty() && !candidate.failure_records.is_empty();
        let matches = gate.status == AgentTaskGateStatus::Failed
            && ((self.status == AgentTaskGateStatus::Failed
                && (opaque || (comparable_records && new.is_empty())))
                || (reproduced_named && new.is_empty()));
        let diagnostic = Some(format!("{}; inherited failures: {:?}; new failures: {:?}; flaky failures: {:?}; base failures: {:?}; base output tail: {}",
            diagnostic.unwrap_or_default(), inherited, new, flaky, self.failure_records, self.output_tail));
        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
            base_ref: base_sha.to_string(),
            exit_code: self.exit_code,
            failure_fingerprint: self.failure_fingerprint.clone(),
            matches_candidate_failure: matches,
            result: if matches {
                AgentTaskGateDifferentialResult::BaselineRed
            } else {
                AgentTaskGateDifferentialResult::CandidateRegression
            },
            diagnostic,
            failure_attribution: (gate.status == AgentTaskGateStatus::Failed).then(|| {
                AgentTaskGateFailureAttribution {
                    classification: if matches {
                        AgentTaskGateFailureOrigin::GateAlreadyRedOnBaseline
                    } else {
                        AgentTaskGateFailureOrigin::CandidateBrokeGate
                    },
                    inherited_failures: inherited,
                    new_failures: new,
                    flaky_failures: flaky,
                }
            }),
        });
        if matches {
            gate.accept_inherited_failure();
        }
    }
}

/// Execute exactly one comparison probe under the recorded invocation contract.
fn replay_comparison_gate(
    gate: &AgentTaskGateReport,
    workspace: &std::path::Path,
    index: usize,
    timeout: std::time::Duration,
    component_id: Option<&str>,
) -> Result<AgentTaskGateReport> {
    let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
    let result = (|| {
        let runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
            &run_dir,
            &homeboy_core::engine::invocation::InvocationRequirements::default(),
        )?;
        let artifacts = gate
            .environment
            .package_artifact_replay_requirements()
            .ok_or_else(|| {
                Error::internal_unexpected("comparison probe has no package artifact contract")
            })?;
        let policy = replay_gate_policy(&gate.environment, component_id);
        let replay = match gate.invocation()? {
            AgentTaskGateInvocation::LegacyShell { command, .. } => run_gate_command_with_timeout(
                workspace,
                index,
                &command,
                gate.visibility,
                gate.reveal_policy,
                &runtime.context().tmp_dir,
                timeout,
                &policy,
                &artifacts,
            ),
            AgentTaskGateInvocation::DeclaredTest { plan } => run_declared_test_with_timeout(
                workspace,
                index,
                &plan,
                gate.visibility,
                gate.reveal_policy,
                &runtime.context().tmp_dir,
                &policy,
                &artifacts,
            ),
        }?;
        if replay.invocation()?.identity_digest()? != gate.invocation()?.identity_digest()?
            || replay.environment.package_artifacts != gate.environment.package_artifacts
        {
            return Err(Error::internal_unexpected(
                "comparison probe changed gate inputs",
            ));
        }
        Ok(replay)
    })();
    run_dir.finish(result.is_ok());
    result
}

fn refine_gate_comparison(
    baseline: &BaselineFacts,
    gate: &mut AgentTaskGateReport,
    candidate_retry: &AgentTaskGateReport,
    baseline_retry: &AgentTaskGateReport,
    base_sha: &str,
) {
    let candidate = BaselineFacts::from_gate(gate);
    let candidate_second = BaselineFacts::from_gate(candidate_retry);
    let mut baseline_second = BaselineFacts::from_gate(baseline_retry);
    if !candidate_second.comparable() || !baseline_second.comparable() {
        if let Some(comparison) = &mut gate.baseline_comparison {
            comparison.result = AgentTaskGateDifferentialResult::Inconclusive;
            comparison.failure_attribution = None;
            comparison.diagnostic =
                Some("bounded flake replay did not produce comparable results".to_string());
        }
        return;
    }
    let mut flaky = Vec::new();
    for (first, second) in [
        (
            &candidate.failure_records,
            &candidate_second.failure_records,
        ),
        (&baseline.failure_records, &baseline_second.failure_records),
    ] {
        flaky.extend(
            first
                .iter()
                .filter(|failure| !second.contains(failure))
                .cloned(),
        );
        flaky.extend(
            second
                .iter()
                .filter(|failure| !first.contains(failure))
                .cloned(),
        );
    }
    flaky.sort();
    flaky.dedup();
    // An outright recovery is a passing gate, but retain the original failure
    // evidence and an explicit receipt: never silently turn red into green.
    if candidate_retry.status == AgentTaskGateStatus::Succeeded {
        let evidence = gate.failure_evidence.clone();
        let mut recovered = candidate_retry.clone();
        recovered.flaky = evidence;
        recovered.baseline_comparison = Some(AgentTaskGateBaselineComparison {
            base_ref: base_sha.to_string(),
            exit_code: baseline.exit_code,
            failure_fingerprint: baseline.failure_fingerprint.clone(),
            matches_candidate_failure: false,
            result: AgentTaskGateDifferentialResult::CandidateImprovement,
            diagnostic: Some(
                "candidate passed bounded comparison replay; flaky-passing gate".to_string(),
            ),
            failure_attribution: Some(AgentTaskGateFailureAttribution {
                classification: AgentTaskGateFailureOrigin::GateAlreadyRedOnBaseline,
                inherited_failures: Vec::new(),
                new_failures: Vec::new(),
                flaky_failures: flaky,
            }),
        });
        *gate = recovered;
        return;
    }
    // Do not excuse a named/opaque mismatch. A failed run with no identities
    // cannot establish that a named failure disappeared.
    let named = !candidate.failure_records.is_empty()
        && !candidate_second.failure_records.is_empty()
        && (!baseline.failure_records.is_empty()
            || !baseline_second.failure_records.is_empty()
            || (baseline.status == AgentTaskGateStatus::Succeeded
                && baseline_second.status == AgentTaskGateStatus::Succeeded));
    if !named {
        if let Some(attribution) = gate
            .baseline_comparison
            .as_mut()
            .and_then(|comparison| comparison.failure_attribution.as_mut())
        {
            attribution.flaky_failures = flaky;
        }
        return;
    }
    let mut stable_candidate = candidate;
    stable_candidate
        .failure_records
        .retain(|failure| candidate_second.failure_records.contains(failure));
    baseline_second
        .failure_records
        .extend(baseline.failure_records.iter().cloned());
    baseline_second.failure_records.sort();
    baseline_second.failure_records.dedup();
    baseline_second.apply_candidate(
        gate,
        base_sha,
        Some("bounded two-sided flake comparison".to_string()),
        &stable_candidate,
        flaky,
        true,
    );
}

/// Observe all declared gates before paid provider work. The cache includes
/// placement and gate inputs, not just the SHA, so hosts never share verdicts.
pub(crate) fn admit_base_gates(
    options: &super::cook::CookRequest,
    store: &ObservationStore,
    base_sha: &str,
) -> Result<Value> {
    let Some(root) = options.workspace.source_worktree_path.as_ref() else {
        return Ok(serde_json::json!([]));
    };
    let workspace =
        super::cook_promotion::component_workspace_path(options)?.unwrap_or_else(|| root.clone());
    let gates = super::cook_recipe::admitted_gate_policy(options)?;
    let mut environment = gates.gate_environment.clone();
    environment.admitted_component_id =
        super::cook::cook_repository_identity_component_id(&options.identity.initial_plan);
    let component = environment.admitted_component_id.clone();
    let inputs = serde_json::json!([root, base_sha, gates]);
    let key = format!(
        "admission-{:x}",
        Sha256::digest(inputs.to_string().as_bytes())
    );
    // Serialize identical admissions across controller processes. Recheck the
    // durable cache only after obtaining the lock; concurrent Cooks must not
    // both pay for the same base observation.
    let lock_root = homeboy_core::artifacts::root()?.join("cook-baseline-locks");
    std::fs::create_dir_all(&lock_root)
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_root.join(&key))
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    lock.lock()
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    if let Some(record) = store.coordination_record(BASELINE_RECORDS, &key)? {
        return Ok(record.payload);
    }
    let results = crate::agent_task_promotion::with_gate_supervision(
        gates.declared_supervision(),
        || {
            with_detached_base_checkout(root, &workspace, base_sha, component.as_deref(), |base| {
                // Repro gates already ran on this base before dispatch, and a
                // base failure never excuses them, so admission skips them.
                let verify: Vec<_> = gates
                    .verify
                    .iter()
                    .filter(|command| !gates.repro.contains(command))
                    .collect();
                let commands: Vec<_> = verify
                    .iter()
                    .copied()
                    .chain(&gates.private_verify)
                    .cloned()
                    .collect();
                hydrate_base_gate_workspace(
                    base,
                    &environment,
                    component.as_deref(),
                    &commands,
                    gates.gate_timeout(),
                )?;
                let mut results = Vec::new();
                for index in 0..(commands.len() + usize::from(gates.test_execution_plan.is_some()))
                {
                    let input = serde_json::json!([
                        root,
                        workspace,
                        base_sha,
                        commands.get(index),
                        if commands.get(index).is_none() {
                            gates.test_execution_plan.as_ref()
                        } else {
                            None
                        },
                        environment,
                        gates.gate_package_artifacts,
                        gates.gate_diagnostic_sidecars,
                        gates.gate_timeout_seconds,
                        gates.gate_no_progress_timeout_seconds,
                        gates.gate_heartbeat_interval_seconds,
                        index < verify.len(),
                        gates.private_gate_reveal,
                    ]);
                    let observation_key = format!(
                        "gate-admission-{:x}",
                        Sha256::digest(input.to_string().as_bytes())
                    );
                    let gate_lock = std::fs::OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(lock_root.join(&observation_key))
                        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
                    gate_lock
                        .lock()
                        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
                    if let Some(record) =
                        store.coordination_record(BASELINE_RECORDS, &observation_key)?
                    {
                        results.push(record.payload);
                        continue;
                    }
                    let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
                    let result = (|| {
                        let runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
                            &run_dir,
                            &homeboy_core::engine::invocation::InvocationRequirements::default(),
                        )?;
                        let visibility = if index < verify.len() || commands.get(index).is_none() {
                            crate::agent_task_gate::AgentTaskGateVisibility::Visible
                        } else {
                            crate::agent_task_gate::AgentTaskGateVisibility::Private
                        };
                        if let Some(command) = commands.get(index) {
                            run_gate_command_with_timeout(
                                base,
                                index + 1,
                                command,
                                visibility,
                                gates.private_gate_reveal,
                                &runtime.context().tmp_dir,
                                gates.gate_timeout(),
                                &environment,
                                &gates.gate_package_artifacts,
                            )
                        } else {
                            run_declared_test_with_timeout(
                                base,
                                index + 1,
                                gates.test_execution_plan.as_ref().unwrap(),
                                visibility,
                                gates.private_gate_reveal,
                                &runtime.context().tmp_dir,
                                &environment,
                                &gates.gate_package_artifacts,
                            )
                        }
                    })();
                    run_dir.finish(result.is_ok());
                    let gate = result?;
                    let facts = BaselineFacts::from_gate(&gate);
                    let payload = serde_json::to_value(&facts)
                        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
                    if facts.comparable() {
                        let gate_key = baseline_record_key(&gate, base_sha, component.as_deref())?;
                        store.put_coordination_record(
                            BASELINE_RECORDS,
                            &gate_key,
                            ExpectedRevision::Absent,
                            RecordWrite {
                                state: "comparable",
                                payload: &payload,
                                expires_at_ms: None,
                            },
                        )?;
                    }
                    let observation = serde_json::json!({"command": gate.command, "status": if gate.status == AgentTaskGateStatus::Failed { "baseline_red" } else if gate.status == AgentTaskGateStatus::Succeeded { "passed" } else { "inconclusive" }, "facts": payload});
                    store.put_coordination_record(
                        BASELINE_RECORDS,
                        &observation_key,
                        ExpectedRevision::Absent,
                        RecordWrite {
                            state: "observed",
                            payload: &observation,
                            expires_at_ms: None,
                        },
                    )?;
                    results.push(observation);
                }
                Ok(Value::Array(results))
            })
        },
    )?;
    store.put_coordination_record(
        BASELINE_RECORDS,
        &key,
        ExpectedRevision::Absent,
        RecordWrite {
            state: "observed",
            payload: &results,
            expires_at_ms: None,
        },
    )?;
    Ok(results)
}

fn baseline_record_key(
    gate: &AgentTaskGateReport,
    base_sha: &str,
    component_id: Option<&str>,
) -> Result<String> {
    // Lab offload metadata is unique per gate run and is observation context,
    // not a gate input; keying on it would make every record unreachable.
    let mut policy = replay_gate_policy(&gate.environment, component_id);
    policy
        .variables
        .remove(homeboy_core::observation::context::LAB_OFFLOAD_METADATA_ENV);
    let inputs = serde_json::json!([
        base_sha,
        gate.invocation()?.identity_digest()?,
        policy,
        gate.environment.package_artifacts,
    ]);
    Ok(format!(
        "{:x}",
        Sha256::digest(inputs.to_string().as_bytes())
    ))
}

/// A failed gate the immutable base may explain. A repro gate is declared to
/// fail on base and pass on the fix, so a base failure never excuses it: a
/// repro still red on the candidate is the candidate's failure, and the Cook
/// retries with it as feedback.
fn awaits_base_comparison(gate: &AgentTaskGateReport, repro: &[String]) -> bool {
    gate.status == AgentTaskGateStatus::Failed
        && gate.baseline_comparison.is_none()
        && !gate
            .invocation()
            .is_ok_and(|invocation| repro.contains(&invocation.reviewer_command()))
}

pub(crate) fn compare_gate_failures_to_verified_base(
    store: &ObservationStore,
    promotion: &mut AgentTaskPromotionReport,
    repro: &[String],
    repository_root: &std::path::Path,
    gate_workspace: &std::path::Path,
    base_sha: &str,
    timeout: std::time::Duration,
    component_id: Option<&str>,
    mut checkpoint: impl FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    if !promotion.status.gate_failed() {
        return Ok(());
    }
    let unresolved = promotion
        .deterministic_gates
        .iter()
        .filter(|gate| awaits_base_comparison(gate, repro))
        .count();
    if unresolved == 0 {
        return Ok(());
    }
    let mut compared = 0;
    for gate in &mut promotion.deterministic_gates {
        if !awaits_base_comparison(gate, repro)
            || gate
                .environment
                .package_artifact_replay_requirements()
                .is_none()
        {
            continue;
        }
        let key = baseline_record_key(gate, base_sha, component_id)?;
        if let Some(record) = store.coordination_record(BASELINE_RECORDS, &key)? {
            if let Ok(facts) = serde_json::from_value::<BaselineFacts>(record.payload) {
                if facts.comparable()
                    && facts.package_artifacts == gate.environment.package_artifacts
                {
                    let mut comparison = gate.clone();
                    facts.apply(
                        &mut comparison,
                        base_sha,
                        Some(format!("reused recorded baseline {key}")),
                    );
                    // A cached regression still needs the bounded two-sided
                    // probe. Do not let its receipt skip that work below.
                    if comparison
                        .baseline_comparison
                        .as_ref()
                        .is_some_and(|result| result.matches_candidate_failure)
                    {
                        *gate = comparison;
                        compared += 1;
                        checkpoint(compared, unresolved)?;
                    }
                }
            }
        }
    }
    if compared == unresolved {
        promotion.normalize_gate_outcome();
        return Ok(());
    }
    with_detached_base_checkout(
        repository_root,
        gate_workspace,
        base_sha,
        component_id,
        |baseline_gate_workspace| {
            let unresolved_gates = || {
                promotion
                    .deterministic_gates
                    .iter()
                    .filter(|gate| awaits_base_comparison(gate, repro))
            };
            let gate_commands: Vec<String> = unresolved_gates()
                .map(|gate| gate.command.join(" "))
                .collect();
            let environment = unresolved_gates()
                .next()
                .map(|gate| replay_gate_policy(&gate.environment, component_id))
                .unwrap_or_default();
            if let Err(error) = hydrate_base_gate_workspace(
                baseline_gate_workspace,
                &environment,
                component_id,
                &gate_commands,
                timeout,
            ) {
                // A base without its dependencies cannot say whether the
                // failure predates the candidate.
                for gate in &mut promotion.deterministic_gates {
                    if awaits_base_comparison(gate, repro) {
                        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                            base_ref: base_sha.to_string(),
                            exit_code: 124,
                            failure_fingerprint: String::new(),
                            matches_candidate_failure: false,
                            result: AgentTaskGateDifferentialResult::Inconclusive,
                            diagnostic: Some(format!("baseline dependency setup failed: {error}")),
                            failure_attribution: None,
                        });
                    }
                }
                promotion.normalize_gate_outcome();
                return Ok(());
            }
            for (index, gate) in promotion.deterministic_gates.iter_mut().enumerate() {
                if !awaits_base_comparison(gate, repro) {
                    continue;
                }
                compared += 1;
                checkpoint(compared, unresolved)?;
                let Some(package_artifacts) =
                    gate.environment.package_artifact_replay_requirements()
                else {
                    gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                    base_ref: base_sha.to_string(),
                    exit_code: 124,
                    failure_fingerprint: String::new(),
                    matches_candidate_failure: false,
                    result: AgentTaskGateDifferentialResult::Inconclusive,
                    diagnostic: Some(
                        "candidate gate declares package artifacts without recorded digests, so the immutable base cannot reconstruct the same inputs"
                            .to_string(),
                    ),
                    failure_attribution: None,
                });
                    continue;
                };
                let invocation = gate.invocation()?;
                let baseline_run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
                let baseline = (|| {
                    let runtime = homeboy_core::engine::invocation::InvocationGuard::acquire(
                        &baseline_run_dir,
                        &homeboy_core::engine::invocation::InvocationRequirements::default(),
                    )?;
                    match &invocation {
                        AgentTaskGateInvocation::LegacyShell { command, .. } => {
                            run_gate_command_with_timeout(
                                baseline_gate_workspace,
                                index + 1,
                                command,
                                gate.visibility,
                                gate.reveal_policy,
                                &runtime.context().tmp_dir,
                                timeout,
                                &replay_gate_policy(&gate.environment, component_id),
                                &package_artifacts,
                            )
                        }
                        AgentTaskGateInvocation::DeclaredTest { plan } => {
                            run_declared_test_with_timeout(
                                baseline_gate_workspace,
                                index + 1,
                                plan,
                                gate.visibility,
                                gate.reveal_policy,
                                &runtime.context().tmp_dir,
                                &replay_gate_policy(&gate.environment, component_id),
                                &package_artifacts,
                            )
                        }
                    }
                })();
                if let Ok(baseline) = &baseline {
                    if baseline.invocation()?.identity_digest()? != invocation.identity_digest()? {
                        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: String::new(),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                        diagnostic: Some(
                            "baseline replay uses a different shell invocation contract than the recorded candidate; recapture candidate evidence"
                                .to_string(),
                        ),
                        failure_attribution: None,
                    });
                        baseline_run_dir.finish(true);
                        continue;
                    }
                    if baseline.environment.package_artifacts != gate.environment.package_artifacts
                    {
                        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: String::new(),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                        diagnostic: Some(
                            "baseline replay resolved different package artifacts than the candidate recorded, so the two runs are not comparable"
                                .to_string(),
                        ),
                        failure_attribution: None,
                    });
                        baseline_run_dir.finish(true);
                        continue;
                    }
                }
                let result: Result<()> = match baseline {
                    Ok(baseline) if baseline.exit_code == 124 => {
                        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                        base_ref: base_sha.to_string(),
                        exit_code: baseline.exit_code,
                        failure_fingerprint: failure_fingerprint(
                            baseline.exit_code,
                            &baseline.stdout,
                            &baseline.stderr,
                            baseline
                                .failure_evidence
                                .as_ref()
                                .map(|evidence| evidence.diagnostics.as_slice())
                                .unwrap_or_default(),
                        ),
                        matches_candidate_failure: false,
                        result: AgentTaskGateDifferentialResult::Inconclusive,
                        diagnostic: Some(
                            "baseline replay exceeded the gate timeout before producing a comparable result"
                                .to_string(),
                        ),
                        failure_attribution: None,
                    });
                        Ok(())
                    }
                    Ok(baseline) => {
                        let facts = BaselineFacts::from_gate(&baseline);
                        facts.apply(gate, base_sha, None);
                        if gate.baseline_comparison.as_ref().is_some_and(|comparison| {
                            comparison.result
                                == AgentTaskGateDifferentialResult::CandidateRegression
                        }) {
                            let retry: Result<(AgentTaskGateReport, AgentTaskGateReport)> =
                                (|| {
                                    let candidate_retry = replay_comparison_gate(
                                        gate,
                                        gate_workspace,
                                        index + 1,
                                        timeout,
                                        component_id,
                                    )?;
                                    let baseline_retry = replay_comparison_gate(
                                        gate,
                                        baseline_gate_workspace,
                                        index + 1,
                                        timeout,
                                        component_id,
                                    )?;
                                    Ok((candidate_retry, baseline_retry))
                                })();
                            match retry {
                                Ok((candidate_retry, baseline_retry)) => {
                                    refine_gate_comparison(
                                        &facts,
                                        gate,
                                        &candidate_retry,
                                        &baseline_retry,
                                        base_sha,
                                    );
                                }
                                Err(error) => {
                                    if let Some(comparison) = &mut gate.baseline_comparison {
                                        comparison.result =
                                            AgentTaskGateDifferentialResult::Inconclusive;
                                        comparison.failure_attribution = None;
                                        comparison.diagnostic =
                                            Some(format!("bounded flake replay failed: {error}"));
                                    }
                                }
                            }
                        }
                        let key = baseline_record_key(gate, base_sha, component_id)?;
                        let payload = serde_json::to_value(&facts).map_err(|error| {
                            Error::internal_io(
                                error.to_string(),
                                Some("serialize baseline facts".to_string()),
                            )
                        })?;
                        if facts.comparable() {
                            store.put_coordination_record(
                                BASELINE_RECORDS,
                                &key,
                                ExpectedRevision::Absent,
                                RecordWrite {
                                    state: "comparable",
                                    payload: &payload,
                                    expires_at_ms: None,
                                },
                            )?;
                        }
                        Ok(())
                    }
                    Err(error) => {
                        gate.baseline_comparison = Some(AgentTaskGateBaselineComparison {
                            base_ref: base_sha.to_string(),
                            exit_code: 124,
                            failure_fingerprint: String::new(),
                            matches_candidate_failure: false,
                            result: AgentTaskGateDifferentialResult::Inconclusive,
                            diagnostic: Some(format!("baseline replay failed: {error}")),
                            failure_attribution: None,
                        });
                        // Preserve the inconclusive evidence and leave the candidate
                        // gate red. A baseline execution failure must never convert a
                        // candidate failure into an infrastructure error that loses
                        // its durable comparison result.
                        Ok(())
                    }
                };
                baseline_run_dir.finish(result.is_ok());
                result?;
            }
            promotion.normalize_gate_outcome();
            Ok(())
        },
    )
}

/// Hydrate the base copy of a gate workspace exactly as candidate gate setup
/// does: the checkout root plus every descendant root a gate names, such as
/// `cd php-transformer && composer test` (#16109), under the gates' own
/// environment. A failed install is an error, never a base to compare against.
pub(crate) fn hydrate_base_gate_workspace(
    workspace: &std::path::Path,
    environment: &crate::agent_task_gate::AgentTaskGateEnvironmentPolicy,
    component_id: Option<&str>,
    gate_commands: &[String],
    timeout: std::time::Duration,
) -> Result<()> {
    // Lab execution hydrates on the runner before running the command. Its
    // recorded extension/data input paths need not exist on this controller.
    if environment.lab_runner.is_some() {
        return Ok(());
    }
    let hydration = crate::agent_task_gate::GateHydrationEnvironment::prepare(environment)?;
    let policy = homeboy_core::deps::DependencyHydrationPolicy {
        timeout,
        environment: hydration.environment(),
        ..Default::default()
    };
    let setup = crate::agent_task_gate::hydrate_gate_dependency_roots_for_component(
        workspace,
        environment.hydrate_dependencies,
        "baseline_gate_workspace",
        &policy,
        component_id,
        Some(gate_commands),
    );
    let failed = setup.as_ref().ok().and_then(|setup| {
        setup
            .iter()
            .find(|outcome| outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed)
            .cloned()
    });
    hydration.finish(setup.is_ok() && failed.is_none());
    setup?;
    match failed {
        Some(failed) => Err(Error::internal_io(
            format!(
                "provider {} on package root {} ({}): {}",
                failed.provider_id,
                failed.package_root,
                failed.reason,
                failed.stderr.trim()
            ),
            Some("hydrate Cook gate baseline dependencies".to_string()),
        )),
        None => Ok(()),
    }
}

/// Materialize the immutable `base_sha` as a detached checkout next to the
/// repository and run `run` in the base copy of `gate_workspace`. `run`
/// hydrates the dependencies its gates need ([`hydrate_base_gate_workspace`]).
/// The checkout is removed before this returns.
pub(crate) fn with_detached_base_checkout<T>(
    repository_root: &std::path::Path,
    gate_workspace: &std::path::Path,
    base_sha: &str,
    component_id: Option<&str>,
    run: impl FnOnce(&std::path::Path) -> Result<T>,
) -> Result<T> {
    let repository_root = repository_root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook gate repository root".to_string()),
        )
    })?;
    let gate_workspace = gate_workspace.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize Cook gate workspace".to_string()),
        )
    })?;
    // A gate baseline is a full checkout plus hydrated dependencies. It
    // belongs on the artifact volume beside the candidate baseline, not in the
    // process temp dir, which is often a small tmpfs (#11128, #17737).
    let baseline_parent = homeboy_core::artifacts::root()?.join("cook-baseline");
    std::fs::create_dir_all(&baseline_parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create Cook gate baseline directory".to_string()),
        )
    })?;
    let baseline_root = tempfile::Builder::new()
        .prefix("gate-baseline-")
        .tempdir_in(&baseline_parent)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create Cook gate baseline".to_string()),
            )
        })?;
    let baseline_path = baseline_root.path().join("base");
    let gate_workspace_relative = gate_workspace.strip_prefix(&repository_root).map_err(|_| {
        Error::validation_invalid_argument(
            "gate_workspace",
            "Cook gate workspace is outside its repository root",
            Some(gate_workspace.display().to_string()),
            None,
        )
    })?;
    let output = std::process::Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&baseline_path)
        .arg(base_sha)
        .current_dir(&repository_root)
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("materialize Cook gate baseline".to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_io(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
            Some("materialize immutable Cook gate baseline".to_string()),
        ));
    }
    let result = (|| -> Result<T> {
        let canonical_baseline_path = baseline_path.canonicalize().map_err(|error| {
            Error::validation_invalid_argument(
                "gate_workspace",
                format!("canonicalize Cook gate baseline root: {error}"),
                Some(baseline_path.display().to_string()),
                None,
            )
        })?;
        let baseline_gate_workspace = homeboy_core::resolve_contained_local_path(
            &canonical_baseline_path,
            gate_workspace_relative,
            "gate_workspace",
        )?;
        let baseline_gate_workspace = baseline_gate_workspace.canonicalize().map_err(|error| {
            Error::validation_invalid_argument(
                "gate_workspace",
                format!("canonicalize Cook baseline component workspace: {error}"),
                Some(baseline_gate_workspace.display().to_string()),
                None,
            )
        })?;
        if !baseline_gate_workspace.starts_with(&canonical_baseline_path) {
            return Err(Error::validation_invalid_argument(
                "gate_workspace",
                "canonical Cook baseline component workspace escapes its baseline root",
                Some(baseline_gate_workspace.display().to_string()),
                None,
            ));
        }
        run(&baseline_gate_workspace)
    })();
    let cleanup = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&baseline_path)
        .current_dir(&repository_root)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("remove Cook gate baseline".to_string()),
            )
        })?;
    if !cleanup.success() {
        return Err(Error::internal_io(
            "git worktree remove failed".to_string(),
            Some("remove Cook gate baseline".to_string()),
        ));
    }
    result
}

/// Re-materialize a follow-up baseline worktree at a specific path that was
/// previously created by [`materialize_follow_up_baseline_in_root`] but has since been
/// reaped (e.g. by tmp cleanup, disk-pressure cleanup, or `git worktree prune`).
///
/// This reuses the same durable promotion data and worktree/patch logic as
/// [`materialize_follow_up_baseline_in_root`] to admit the current target at the
/// original path. Historical identity is retained as drift evidence; the new
/// capability binds provider preflight to the newly admitted immutable tree.
pub(crate) fn re_materialize_follow_up_baseline(
    promotion: &AgentTaskPromotionReport,
    target_path: &std::path::Path,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    materialize_follow_up_baseline_at(
        promotion,
        None,
        Some(target_path),
        source_run_id,
        bound_task_id,
    )
}

/// Shared worktree/patch materialization logic. When `target_path` is `None` a
/// fresh UUID path is generated under the follow-up baselines temp directory;
/// when `Some(path)` the worktree is created at the exact given path (used by
/// recovery to re-materialize a reaped baseline at its original location).
fn materialize_follow_up_baseline_at(
    promotion: &AgentTaskPromotionReport,
    artifact_root: Option<&std::path::Path>,
    target_path: Option<&std::path::Path>,
    source_run_id: &str,
    bound_task_id: &str,
) -> Result<CookFollowUpBaseline> {
    let source_root = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "gate-failed promotion did not report its managed target workspace",
                None,
                None,
            )
        })?;
    let expected_head = promotion.target.head.as_deref();
    let parent_snapshot = parent_snapshot_from_current_process()?;
    // Historical identity is evidence, not authority over today's target.
    let recorded_candidate = promotion.provenance.get("candidate_commit");
    let immutable = crate::agent_task_candidate_tree::current_workspace_candidate(&source_root)?;
    let drift = serde_json::json!({
        "recorded_target_head": expected_head,
        "recorded_candidate": recorded_candidate,
        "admitted_candidate": immutable,
    });
    let path = match target_path {
        Some(p) => p.to_path_buf(),
        None => {
            // Same reasoning as the initial-candidate baseline above: a whole
            // working-tree checkout belongs under the artifact root, not in
            // the process temp dir where nothing can see or reap it (#11128).
            let root = match artifact_root {
                Some(root) => root.to_path_buf(),
                None => homeboy_core::artifacts::root()?,
            };
            let parent = root.join("cook-baseline");
            std::fs::create_dir_all(&parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("create cook baseline directory".to_string()),
                )
            })?;
            parent.join(format!("baseline-{}", uuid::Uuid::new_v4()))
        }
    };
    let path_string = path.display().to_string();
    git_output(
        &source_root,
        &[
            "worktree",
            "add",
            "--detach",
            &path_string,
            &immutable.candidate_sha,
        ],
    )?;
    let baseline = CookFollowUpBaseline {
        source_root,
        path: path.clone(),
        // The capability is completed only after the committed baseline's
        // identity has been verified below.
        capability: DerivedCookBaselineCapability {
            canonical_path: path,
            commit: String::new(),
            tree: String::new(),
            source_run_id: source_run_id.to_string(),
            source_task_id: promotion.source.task_id.clone(),
            bound_task_id: bound_task_id.to_string(),
            parent_snapshot,
            preexisting_candidate: false,
            baseline_admission: Some(drift),
        },
    };
    let (commit, tree) = (immutable.candidate_sha, immutable.tree_sha);
    let mut baseline = baseline;
    baseline.capability.canonical_path = baseline.path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize cook retry baseline".to_string()),
        )
    })?;
    baseline.capability.commit = commit;
    baseline.capability.tree = tree;
    Ok(baseline)
}

fn parent_snapshot_from_current_process() -> Result<Option<Value>> {
    let Some(raw) = std::env::var(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV).ok()
    else {
        return Ok(None);
    };
    parent_snapshot_from_transport(&raw).map(Some)
}

fn parent_snapshot_from_transport(raw: &str) -> Result<Value> {
    homeboy_core::observation::resolve_json_value(raw).ok_or_else(|| {
        Error::validation_invalid_argument(
            "source_snapshot",
            "invalid inline or referenced source snapshot JSON",
            None,
            None,
        )
    })
}

pub(crate) fn git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    git_output_with_env(cwd, args, &[])
}

pub(crate) fn git_output_with_env(
    cwd: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("git {}", args.join(" "))))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lab_baseline_hydration_does_not_resolve_runner_environment_on_controller() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().unwrap();
            let mut environment = crate::agent_task_gate::AgentTaskGateEnvironmentPolicy::default();
            environment.preserve.insert(
                "RUNNER_TOOL_HOME".into(),
                "HOMEBOY_TEST_RUNNER_ONLY_TOOL_18243/toolchain".into(),
            );
            // This mapping is available only on the runner. Local preparation
            // fails; Lab replay must leave its resolution to Lab execution.
            assert!(hydrate_base_gate_workspace(
                workspace.path(),
                &environment,
                None,
                &[],
                std::time::Duration::from_secs(1),
            )
            .is_err());
            environment.lab_runner = Some("lab".into());
            hydrate_base_gate_workspace(
                workspace.path(),
                &environment,
                None,
                &[],
                std::time::Duration::from_secs(1),
            )
            .unwrap();
        });
    }

    use super::*;
    use crate::agent_task_gate::{
        AgentTaskGateEnvironment, AgentTaskGateFailureClassification, AgentTaskGateFailureEvidence,
        AgentTaskGateRevealPolicy, AgentTaskGateStatus,
    };
    use homeboy_core::gate::HomeboyGateVisibility;
    use sha2::{Digest, Sha256};

    #[test]
    fn baseline_fixture_gate_attributes_only_new_test_failures() {
        let workspace = tempfile::tempdir().unwrap();
        let run = |failures: &str| {
            run_gate_command_with_timeout(
                workspace.path(),
                1,
                &format!("printf '{failures}'; exit 1"),
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                workspace.path(),
                std::time::Duration::from_secs(10),
                &Default::default(),
                &[],
            )
            .unwrap()
        };
        let base = BaselineFacts::from_gate(&run("test A ... FAILED\\n"));
        let mut inherited = run("test A ... FAILED\\n");
        base.apply(&mut inherited, "base", None);
        assert_eq!(
            inherited.status,
            AgentTaskGateStatus::AcceptedInheritedFailure
        );
        let attribution = inherited
            .baseline_comparison
            .as_ref()
            .and_then(|comparison| comparison.failure_attribution.clone())
            .expect("inherited failure carries attribution");
        assert_eq!(
            attribution.classification,
            AgentTaskGateFailureOrigin::GateAlreadyRedOnBaseline
        );
        assert_eq!(attribution.inherited_failures.len(), 1);
        assert!(attribution.new_failures.is_empty());
        let mut regression = run("test A ... FAILED\\ntest B ... FAILED\\n");
        base.apply(&mut regression, "base", None);
        assert_eq!(regression.status, AgentTaskGateStatus::Failed);
        let comparison = regression.baseline_comparison.unwrap();
        assert_eq!(
            comparison.result,
            AgentTaskGateDifferentialResult::CandidateRegression
        );
        assert!(comparison.diagnostic.unwrap().contains("B"));
        let attribution = comparison
            .failure_attribution
            .expect("regression attribution");
        assert_eq!(
            attribution.classification,
            AgentTaskGateFailureOrigin::CandidateBrokeGate
        );
        assert_eq!(attribution.inherited_failures.len(), 1);
        assert_eq!(attribution.new_failures.len(), 1);
        assert!(attribution.new_failures[0].contains('B'));
        let opaque = BaselineFacts::from_gate(&run("host unavailable"));
        let mut candidate = run("different host error");
        opaque.apply(&mut candidate, "base", None);
        assert_eq!(
            candidate.status,
            AgentTaskGateStatus::AcceptedInheritedFailure
        );
        // An opaque baseline cannot excuse a candidate that names failures:
        // nothing on the baseline accounts for them.
        let mut named = run("test C ... FAILED\\n");
        opaque.apply(&mut named, "base", None);
        assert_eq!(named.status, AgentTaskGateStatus::Failed);
        let attribution = named
            .baseline_comparison
            .unwrap()
            .failure_attribution
            .expect("named regression attribution");
        assert_eq!(
            attribution.classification,
            AgentTaskGateFailureOrigin::CandidateBrokeGate
        );
        assert_eq!(attribution.new_failures.len(), 1);
        // A baseline with named failures cannot excuse an opaque candidate
        // failure, which may be a new compile or harness break.
        let mut unnamed = run("compile error");
        base.apply(&mut unnamed, "base", None);
        assert_eq!(unnamed.status, AgentTaskGateStatus::Failed);
    }

    #[test]
    fn bounded_comparison_separates_stable_new_inherited_and_flaky_failures() {
        let gate = |names: &[&str], exit_code| {
            let mut gate = AgentTaskGateReport::new(
                "probe",
                crate::agent_task_gate::legacy_gate_argv("tests"),
                exit_code,
                names
                    .iter()
                    .map(|name| format!("test {name} ... FAILED\n"))
                    .collect::<String>(),
                "",
                None,
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                AgentTaskGateEnvironment::default(),
            );
            if exit_code != 0 {
                gate.failure_evidence = Some(AgentTaskGateFailureEvidence {
                    classification: AgentTaskGateFailureClassification::CandidateCode,
                    summary: "tests failed".into(),
                    command: "tests".into(),
                    exit_code,
                    stdout_tail: gate.stdout.clone(),
                    stderr_tail: String::new(),
                    failed_tests: Vec::new(),
                    agent_feedback: String::new(),
                    diagnostics: Vec::new(),
                });
            }
            gate
        };
        let base = BaselineFacts::from_gate(&gate(&["a", "b"], 1));
        let mut candidate = gate(&["a", "b", "c"], 1);
        base.apply(&mut candidate, "base", None);
        refine_gate_comparison(
            &base,
            &mut candidate,
            &gate(&["a", "c"], 1),
            &gate(&["a"], 1),
            "base",
        );
        let attribution = candidate
            .baseline_comparison
            .as_ref()
            .unwrap()
            .failure_attribution
            .as_ref()
            .unwrap();
        assert_eq!(attribution.new_failures, vec!["test:c"]);
        assert_eq!(attribution.inherited_failures, vec!["test:a"]);
        assert_eq!(attribution.flaky_failures, vec!["test:b"]);
        assert_eq!(candidate.status, AgentTaskGateStatus::Failed);

        // A candidate-only intermittent failure does not block promotion.
        let mut candidate = gate(&["a", "c"], 1);
        base.apply(&mut candidate, "base", None);
        refine_gate_comparison(
            &base,
            &mut candidate,
            &gate(&["a"], 1),
            &gate(&["a"], 1),
            "base",
        );
        assert_eq!(
            candidate.status,
            AgentTaskGateStatus::AcceptedInheritedFailure
        );
        assert!(candidate
            .baseline_comparison
            .unwrap()
            .failure_attribution
            .unwrap()
            .new_failures
            .is_empty());

        // A failure first observed on the baseline's second run belongs to
        // the baseline union, even if it reproduces on the candidate.
        let mut candidate = gate(&["a", "c"], 1);
        base.apply(&mut candidate, "base", None);
        refine_gate_comparison(
            &base,
            &mut candidate,
            &gate(&["a", "c"], 1),
            &gate(&["a", "c"], 1),
            "base",
        );
        assert_eq!(
            candidate.status,
            AgentTaskGateStatus::AcceptedInheritedFailure
        );
        let attribution = candidate
            .baseline_comparison
            .unwrap()
            .failure_attribution
            .unwrap();
        assert!(attribution.new_failures.is_empty());
        assert!(attribution.flaky_failures.contains(&"test:c".to_string()));

        // Opaque failures that recover are visibly flaky-passing.
        let mut candidate = gate(&[], 1);
        base.apply(&mut candidate, "base", None);
        refine_gate_comparison(
            &base,
            &mut candidate,
            &gate(&[], 0),
            &gate(&["a"], 1),
            "base",
        );
        assert_eq!(candidate.status, AgentTaskGateStatus::Succeeded);
        assert!(candidate.flaky.is_some());
        assert!(candidate
            .baseline_comparison
            .unwrap()
            .diagnostic
            .unwrap()
            .contains("flaky-passing"));

        // A timeout cannot erase a real regression.
        let mut candidate = gate(&["a", "c"], 1);
        base.apply(&mut candidate, "base", None);
        refine_gate_comparison(
            &base,
            &mut candidate,
            &gate(&["a"], 124),
            &gate(&["a"], 1),
            "base",
        );
        assert_eq!(candidate.status, AgentTaskGateStatus::Failed);
        assert_eq!(
            candidate.baseline_comparison.unwrap().result,
            AgentTaskGateDifferentialResult::Inconclusive
        );
    }

    #[test]
    fn vitest_failure_attribution_uses_base_failure_subset() {
        let failures = [
            " × fluid readiness > keeps hero fluid 12ms\n FAIL  src/lib/fluid-readiness.test.ts > fluid readiness > keeps hero fluid\nAssertionError: expected true to be false\n",
            " ✕ viewport class states > preserves disclosure baseline (8 ms)\n FAIL  src/lib/viewport-class-states.test.ts > viewport class states > preserves disclosure baseline\nAssertionError: expected hidden to be visible\n",
            " × selectable set > captures selection 3ms\n FAIL  src/lib/selectable-set-capture.test.ts > selectable set > captures selection\nAssertionError: missing selection\n",
        ];
        let output = |indices: &[usize], extra_pass: bool| {
            let mut output = String::from(" RUN  v3.2.4 /workspace/data-liberation-agent\n");
            for index in indices {
                output.push_str(failures[*index]);
            }
            if extra_pass {
                output.push_str(" ✓ src/lib/new-passing.test.ts (1 test) 5ms\n");
            }
            output.push_str(&format!(
                " Test Files  {} failed | {} passed ({})\n      Tests  {} failed | {} passed ({})\n Duration  2.34s\n",
                indices.len(), 81 + usize::from(extra_pass), 81 + indices.len() + usize::from(extra_pass),
                indices.len(), 1204 + usize::from(extra_pass), 1204 + indices.len() + usize::from(extra_pass),
            ));
            output
        };
        let gate = |output: String| {
            AgentTaskGateReport::new(
                "vitest",
                crate::agent_task_gate::legacy_gate_argv("npx vitest run"),
                1,
                output,
                "",
                None,
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                AgentTaskGateEnvironment::default(),
            )
        };
        let base = BaselineFacts::from_gate(&gate(output(&[0, 1], false)));
        // Exercise the durable cache as well as a freshly captured baseline.
        let base: BaselineFacts =
            serde_json::from_value(serde_json::to_value(base).unwrap()).unwrap();
        for (name, indices, expected) in [
            (
                "extra passing file",
                vec![0, 1],
                AgentTaskGateDifferentialResult::BaselineRed,
            ),
            (
                "new failure",
                vec![0, 1, 2],
                AgentTaskGateDifferentialResult::CandidateRegression,
            ),
            (
                "fixed one failure",
                vec![0],
                AgentTaskGateDifferentialResult::BaselineRed,
            ),
        ] {
            let mut candidate = gate(output(&indices, true).replace("12ms", "99ms"));
            if indices == [0, 1] {
                assert_eq!(
                    base.failure_fingerprint,
                    BaselineFacts::from_gate(&candidate).failure_fingerprint,
                    "{name}"
                );
            }
            candidate.failure_evidence = Some(AgentTaskGateFailureEvidence {
                classification: AgentTaskGateFailureClassification::CandidateCode,
                summary: "vitest failed".into(),
                command: "npx vitest run".into(),
                exit_code: 1,
                stdout_tail: candidate.stdout.clone(),
                stderr_tail: String::new(),
                failed_tests: Vec::new(),
                agent_feedback: "Repair the candidate failure.".into(),
                diagnostics: Vec::new(),
            });
            base.apply(&mut candidate, "base", None);
            assert_eq!(
                candidate.baseline_comparison.as_ref().unwrap().result,
                expected,
                "{name}"
            );
            assert_eq!(
                candidate.status == AgentTaskGateStatus::AcceptedInheritedFailure,
                expected == AgentTaskGateDifferentialResult::BaselineRed,
                "{name}"
            );
        }
    }

    /// A no-change retry (#17960) builds on the unchanged target HEAD: the
    /// attempt it follows recorded no candidate commit because it made none.
    #[test]
    fn no_change_promotion_materializes_its_target_head_as_the_retry_baseline() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let temp = tempfile::tempdir().expect("repository");
            let root = temp.path();
            git_output(root, &["init", "-b", "main"]).expect("init");
            git_output(root, &["config", "user.name", "Homeboy Test"]).expect("name");
            git_output(root, &["config", "user.email", "test@example.test"]).expect("email");
            std::fs::write(root.join("README"), "base\n").expect("base file");
            git_output(root, &["add", "README"]).expect("add");
            git_output(root, &["commit", "-m", "base"]).expect("commit");
            let head = git_output(root, &["rev-parse", "HEAD"]).expect("head");
            let promotion: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "no_changes",
                "source": {"kind": "aggregate", "task_id": "task", "run_id": "run-a"},
                "to_worktree": "repo@fix",
                "target": {"worktree": "repo@fix", "path": root, "head": head},
                "changed_files": [],
                "operator_notification": {"status": "completed", "message": "no changes"},
                "provenance": {"worktree_path": root},
            }))
            .expect("promotion");

            let baseline =
                materialize_follow_up_baseline_in_root(&promotion, home.path(), "run-a", "task-2")
                    .expect("a no-change promotion yields a retry baseline");
            assert_eq!(baseline.capability.commit, head);
            assert_eq!(
                std::fs::read_to_string(baseline.path.join("README")).unwrap(),
                "base\n"
            );
        });
    }

    /// A follow-up baseline handed to a detached dispatcher must outlive the
    /// dispatch call: Lab staging materializes it from a separate controller
    /// job after `dispatch_attempt` returns. `preserve_for_retry` keeps the
    /// checkout; dropping it removes the checkout.
    #[test]
    fn preserved_baseline_outlives_its_owner_and_dropped_baseline_is_removed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repository");
            let root = temp.path();
            git_output(root, &["init", "-b", "main"]).expect("init");
            git_output(root, &["config", "user.name", "Homeboy Test"]).expect("name");
            git_output(root, &["config", "user.email", "test@example.test"]).expect("email");
            std::fs::write(root.join("README"), "base\n").expect("base file");
            git_output(root, &["add", "README"]).expect("add");
            git_output(root, &["commit", "-m", "base"]).expect("commit");

            std::fs::write(root.join("README"), "candidate\n").expect("candidate");
            let plan: AgentTaskPlan = serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "baseline-lifetime",
                "tasks": [{
                    "schema": crate::agent_task::AGENT_TASK_REQUEST_SCHEMA,
                    "task_id": "task",
                    "executor": {"backend": "test"},
                    "instructions": "fixture",
                }],
            }))
            .expect("plan");
            let preserved = materialize_initial_candidate_baseline(&plan, Some(root), "run-a")
                .expect("materialize")
                .expect("dirty candidate");
            let preserved_path = preserved.path.clone();
            preserved.preserve_for_retry();
            assert!(
                preserved_path.join("README").is_file(),
                "a preserved baseline stays on disk for the dispatched attempt"
            );

            let dropped = materialize_initial_candidate_baseline(&plan, Some(root), "run-b")
                .expect("materialize")
                .expect("dirty candidate");
            let dropped_path = dropped.path.clone();
            drop(dropped);
            assert!(
                !dropped_path.exists(),
                "an owned baseline is removed on drop"
            );
        });
    }

    #[test]
    fn cook_baseline_resolves_referenced_parent_snapshot() {
        let directory = tempfile::tempdir().expect("transport directory");
        let path = directory.path().join("source.json");
        let payload = br#"{"schema":"fixture/source/v1","snapshot_hash":"abc"}"#;
        std::fs::write(&path, payload).expect("write source snapshot");
        let reference = serde_json::json!({
            "schema": homeboy_core::observation::PROVENANCE_REFERENCE_SCHEMA,
            "path": path,
            "sha256": format!("{:x}", Sha256::digest(payload)),
        })
        .to_string();

        assert_eq!(
            parent_snapshot_from_transport(&reference).expect("referenced parent snapshot")
                ["snapshot_hash"],
            "abc"
        );
    }

    #[test]
    fn differential_gate_comparison_classifies_inherited_regression_and_inconclusive() {
        homeboy_core::test_support::with_isolated_home(|_| {
            for (name, command, candidate_output, timeout, expected, accepted) in [
                (
                    "inherited",
                    "printf 'same\\n' >&2; exit 1",
                    "same\n",
                    std::time::Duration::from_secs(1),
                    AgentTaskGateDifferentialResult::BaselineRed,
                    true,
                ),
                (
                    "opaque_red",
                    "cat state >&2; exit 1",
                    "candidate\n",
                    std::time::Duration::from_secs(1),
                    AgentTaskGateDifferentialResult::BaselineRed,
                    true,
                ),
                // The counter assertion needs the shell to record its run
                // before the timeout kills it. A 10ms timeout could fire
                // before the `echo` on a loaded runner (#17410).
                (
                    "inconclusive",
                    "sleep 5; exit 1",
                    "candidate\n",
                    std::time::Duration::from_secs(1),
                    AgentTaskGateDifferentialResult::Inconclusive,
                    false,
                ),
            ] {
                let temp = tempfile::tempdir().expect("repository");
                git_output(temp.path(), &["init", "-b", "main"]).expect("init");
                git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
                git_output(temp.path(), &["config", "user.email", "test@example.test"])
                    .expect("email");
                std::fs::write(temp.path().join("state"), "base\n").expect("base state");
                git_output(temp.path(), &["add", "state"]).expect("add");
                git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
                let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");
                std::fs::write(temp.path().join("state"), "candidate\n").expect("candidate state");
                let mut promotion: AgentTaskPromotionReport =
                    serde_json::from_value(serde_json::json!({
                        "schema": "homeboy/agent-task-promotion-report/v1",
                        "status": "gate_failed",
                        "source": {"kind": "aggregate", "task_id": "task"},
                        "to_worktree": "fixture",
                        "target": {"worktree": "fixture", "path": temp.path()},
                        "candidate": {"base_sha": "base", "candidate_sha": "patch", "tree_sha": "tree"},
                        "deterministic_gates": [],
                        "operator_notification": {"status": "blocked", "message": "red"}
                    }))
                    .expect("promotion");
                let counter = temp.path().join("baseline-count");
                let command = format!("echo run >> '{}'; {command}", counter.display());
                let mut gate = crate::agent_task_gate::AgentTaskGateReport::new(
                    name,
                    crate::agent_task_gate::legacy_gate_argv(&command),
                    1,
                    "",
                    candidate_output,
                    None,
                    HomeboyGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                    AgentTaskGateEnvironment::default(),
                );
                gate.status = AgentTaskGateStatus::Failed;
                gate.failure_evidence = Some(AgentTaskGateFailureEvidence {
                    classification: AgentTaskGateFailureClassification::CandidateCode,
                    summary: "candidate gate failed".to_string(),
                    command: command.to_string(),
                    exit_code: 1,
                    stdout_tail: String::new(),
                    stderr_tail: candidate_output.to_string(),
                    failed_tests: Vec::new(),
                    agent_feedback: "Repair the candidate gate failure.".to_string(),
                    diagnostics: Vec::new(),
                });
                promotion.deterministic_gates.push(gate);

                let store =
                    ObservationStore::open_initialized_at(temp.path().join("observations.sqlite"))
                        .expect("store");
                let original = promotion.clone();
                let result = compare_gate_failures_to_verified_base(
                    &store,
                    &mut promotion,
                    &[],
                    temp.path(),
                    temp.path(),
                    &base,
                    timeout,
                    None,
                    |_compared, _total| Ok(()),
                );

                result.expect("baseline comparison");
                drop(store);
                let store =
                    ObservationStore::open_initialized_at(temp.path().join("observations.sqlite"))
                        .expect("reopen durable store");
                let mut second = original.clone();
                // A hit needs neither a valid repository nor a workspace: lookup
                // precedes worktree creation and dependency materialization.
                let replay_root = if accepted {
                    temp.path().join("absent")
                } else {
                    temp.path().to_path_buf()
                };
                compare_gate_failures_to_verified_base(
                    &store,
                    &mut second,
                    &[],
                    &replay_root,
                    &replay_root,
                    &base,
                    timeout,
                    None,
                    |_, _| Ok(()),
                )
                .expect("second comparison");
                assert_eq!(
                    second.deterministic_gates[0].status,
                    promotion.deterministic_gates[0].status
                );
                assert_eq!(
                    second.deterministic_gates[0]
                        .baseline_comparison
                        .as_ref()
                        .unwrap()
                        .result,
                    expected
                );
                let runs = || {
                    std::fs::read_to_string(&counter)
                        .expect("counter")
                        .lines()
                        .count()
                };
                assert_eq!(
                    runs(),
                    if accepted || name == "regression" {
                        1
                    } else {
                        2
                    }
                );
                if accepted {
                    assert!(second.deterministic_gates[0]
                        .baseline_comparison
                        .as_ref()
                        .unwrap()
                        .diagnostic
                        .as_ref()
                        .unwrap()
                        .contains("reused"));
                    let mut different_failure = original.clone();
                    different_failure.deterministic_gates[0].stderr =
                        "different candidate failure\n".to_string();
                    compare_gate_failures_to_verified_base(
                        &store,
                        &mut different_failure,
                        &[],
                        &replay_root,
                        &replay_root,
                        &base,
                        timeout,
                        None,
                        |_, _| Ok(()),
                    )
                    .expect("reuse with different candidate failure");
                    assert_eq!(
                        different_failure.deterministic_gates[0]
                            .baseline_comparison
                            .as_ref()
                            .unwrap()
                            .result,
                        AgentTaskGateDifferentialResult::BaselineRed
                    );
                    assert_eq!(runs(), 1);
                    let mut changed = original.clone();
                    changed.deterministic_gates[0].command =
                        crate::agent_task_gate::legacy_gate_argv(&format!("{command}; # changed"));
                    changed.deterministic_gates[0].invocation = None;
                    compare_gate_failures_to_verified_base(
                        &store,
                        &mut changed,
                        &[],
                        temp.path(),
                        temp.path(),
                        &base,
                        timeout,
                        None,
                        |_, _| Ok(()),
                    )
                    .expect("changed command");
                    assert_eq!(runs(), 2);
                    git_output(temp.path(), &["add", "state"]).expect("add changed state");
                    git_output(temp.path(), &["commit", "-m", "new base"]).expect("new base");
                    let new_base =
                        git_output(temp.path(), &["rev-parse", "HEAD"]).expect("new sha");
                    let mut changed = original.clone();
                    compare_gate_failures_to_verified_base(
                        &store,
                        &mut changed,
                        &[],
                        temp.path(),
                        temp.path(),
                        &new_base,
                        timeout,
                        None,
                        |_, _| Ok(()),
                    )
                    .expect("changed base");
                    assert_eq!(runs(), 3);

                    // The same invocation/base with new package bytes must not
                    // borrow the earlier result. Both paths are committed files.
                    let mut packaged = original.clone();
                    packaged.deterministic_gates[0]
                        .environment
                        .package_artifacts = vec![AgentTaskGatePackageArtifactProvenance {
                        id: "fixture".to_string(),
                        environment: "FIXTURE_PACKAGE".to_string(),
                        value: "fixture".to_string(),
                        source: None,
                        artifacts: vec![
                            crate::agent_task_gate::AgentTaskGateArtifactPathProvenance {
                                path: "state".to_string(),
                                sha256: Some(format!("sha256:{:x}", Sha256::digest(b"base\n"))),
                            },
                        ],
                        remediation: serde_json::json!({"action": "restore fixture"}),
                    }];
                    compare_gate_failures_to_verified_base(
                        &store,
                        &mut packaged,
                        &[],
                        temp.path(),
                        temp.path(),
                        &base,
                        timeout,
                        None,
                        |_, _| Ok(()),
                    )
                    .expect("package comparison");
                    assert_eq!(runs(), 4);
                    let mut changed = original.clone();
                    changed.deterministic_gates[0].environment.package_artifacts = packaged
                        .deterministic_gates[0]
                        .environment
                        .package_artifacts
                        .clone();
                    changed.deterministic_gates[0].environment.package_artifacts[0].artifacts[0]
                        .sha256 = Some("sha256:changed".to_string());
                    compare_gate_failures_to_verified_base(
                        &store,
                        &mut changed,
                        &[],
                        temp.path(),
                        temp.path(),
                        &base,
                        timeout,
                        None,
                        |_, _| Ok(()),
                    )
                    .expect("changed package digest");
                    let comparison = changed.deterministic_gates[0]
                        .baseline_comparison
                        .as_ref()
                        .unwrap();
                    assert_eq!(
                        comparison.result,
                        AgentTaskGateDifferentialResult::Inconclusive
                    );
                    assert!(!comparison.diagnostic.as_ref().unwrap().contains("reused"));
                }
                assert_eq!(
                    promotion.deterministic_gates[0]
                        .baseline_comparison
                        .as_ref()
                        .expect("comparison")
                        .result,
                    expected
                );
                assert_eq!(
                    promotion.deterministic_gates[0].status
                        == AgentTaskGateStatus::AcceptedInheritedFailure,
                    accepted
                );
                assert_eq!(
                    promotion.status,
                    if accepted {
                        crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
                    } else {
                        crate::agent_task_promotion::AgentTaskPromotionStatus::GateFailed
                    }
                );
            }
        });
    }

    /// Production gates each carry a unique Lab offload context; the record key
    /// must ignore it, or no baseline record is ever reused (#16604).
    #[test]
    fn baseline_record_key_ignores_lab_offload_context_but_not_gate_inputs() {
        let gate = |offload: &str, path: &str| {
            let environment = AgentTaskGateEnvironment {
                inherited: vec![
                    crate::agent_task_gate::AgentTaskGateEnvironmentVariable {
                        name: homeboy_core::observation::context::LAB_OFFLOAD_METADATA_ENV
                            .to_string(),
                        value: offload.to_string(),
                    },
                    crate::agent_task_gate::AgentTaskGateEnvironmentVariable {
                        name: "PATH".to_string(),
                        value: path.to_string(),
                    },
                ],
                ..AgentTaskGateEnvironment::default()
            };
            crate::agent_task_gate::AgentTaskGateReport::new(
                "gate",
                crate::agent_task_gate::legacy_gate_argv("exit 1"),
                1,
                "",
                "",
                None,
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                environment,
            )
        };
        let key = |gate: &crate::agent_task_gate::AgentTaskGateReport, base: &str| {
            baseline_record_key(gate, base, Some("component")).expect("key")
        };
        let first = gate("{\"run\":1}", "/usr/bin");
        assert_eq!(
            key(&first, "base"),
            key(&gate("{\"run\":2}", "/usr/bin"), "base")
        );
        assert_ne!(
            key(&first, "base"),
            key(&gate("{\"run\":1}", "/opt/bin"), "base")
        );
        assert_ne!(key(&first, "base"), key(&first, "moved"));
    }

    /// An identical base failure excuses an ordinary gate, but never a repro
    /// gate: a repro is declared red on base, so still failing on the
    /// candidate means the fix did not land and the Cook must retry.
    #[test]
    fn differential_gate_comparison_accepts_an_identical_failure_except_for_a_repro() {
        let command = "cd packages/missing-component && cargo test";
        for repro in [Vec::new(), vec![command.to_string()]] {
            homeboy_core::test_support::with_isolated_home(|_| {
                let temp = tempfile::tempdir().expect("repository");
                git_output(temp.path(), &["init", "-b", "main"]).expect("init");
                git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
                git_output(temp.path(), &["config", "user.email", "test@example.test"])
                    .expect("email");
                std::fs::write(temp.path().join("README"), "base\n").expect("base file");
                git_output(temp.path(), &["add", "README"]).expect("add");
                git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
                let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");

                let candidate = std::process::Command::new("sh")
                    .args(&crate::agent_task_gate::legacy_gate_argv(command)[1..])
                    .current_dir(temp.path())
                    .output()
                    .expect("run candidate gate");
                assert!(!candidate.status.success());
                let mut promotion: AgentTaskPromotionReport =
                serde_json::from_value(serde_json::json!({
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "gate_failed",
                    "source": {"kind": "aggregate", "task_id": "task"},
                    "to_worktree": "fixture",
                    "target": {"worktree": "fixture", "path": temp.path()},
                    "candidate": {"base_sha": "base", "candidate_sha": "patch", "tree_sha": "tree"},
                    "deterministic_gates": [],
                    "operator_notification": {"status": "blocked", "message": "red"}
                }))
                .expect("promotion");
                let stdout = String::from_utf8_lossy(&candidate.stdout).to_string();
                let stderr = String::from_utf8_lossy(&candidate.stderr).to_string();
                let exit_code = candidate.status.code().unwrap_or(1);
                let mut gate = crate::agent_task_gate::AgentTaskGateReport::new(
                    "monorepo-gate",
                    crate::agent_task_gate::legacy_gate_argv(command),
                    exit_code,
                    &stdout,
                    &stderr,
                    None,
                    HomeboyGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                    AgentTaskGateEnvironment::default(),
                );
                gate.failure_evidence = Some(AgentTaskGateFailureEvidence {
                    classification: AgentTaskGateFailureClassification::CandidateCode,
                    summary: "monorepo gate failed from its requested path".to_string(),
                    command: command.to_string(),
                    exit_code,
                    stdout_tail: stdout,
                    stderr_tail: stderr,
                    failed_tests: Vec::new(),
                    agent_feedback: "Repair the candidate gate failure.".to_string(),
                    diagnostics: Vec::new(),
                });
                promotion.deterministic_gates.push(gate);

                compare_gate_failures_to_verified_base(
                    &ObservationStore::open_initialized_at(temp.path().join("observations.sqlite"))
                        .expect("store"),
                    &mut promotion,
                    &repro,
                    temp.path(),
                    temp.path(),
                    &base,
                    std::time::Duration::from_secs(1),
                    None,
                    |_compared, _total| Ok(()),
                )
                .expect("baseline comparison");

                let gate = &promotion.deterministic_gates[0];
                if repro.is_empty() {
                    assert_eq!(
                        gate.status,
                        AgentTaskGateStatus::AcceptedInheritedFailure,
                        "{gate:#?}"
                    );
                    assert_eq!(
                        gate.baseline_comparison
                            .as_ref()
                            .expect("comparison")
                            .result,
                        AgentTaskGateDifferentialResult::BaselineRed
                    );
                } else {
                    assert_eq!(gate.status, AgentTaskGateStatus::Failed, "{gate:#?}");
                    assert!(gate.baseline_comparison.is_none(), "{gate:#?}");
                    assert!(promotion.status.gate_failed());
                }
            });
        }
    }

    /// The base copy of the gate workspace gets the same dependency setup as
    /// the candidate: a gate that runs inside a nested dependency root
    /// (`cd tool && ...`) replays on the base with that root hydrated, so an
    /// inherited failure compares as inherited rather than as a missing
    /// install.
    #[test]
    fn differential_gate_comparison_hydrates_the_dependency_roots_its_gates_name() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let adapters = home
                .path()
                .join(".config/homeboy/extensions/dependency-adapters");
            std::fs::create_dir_all(&adapters).expect("adapter directory");
            std::fs::write(
                adapters.join("index.json"),
                r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"fixture","ecosystem":"fixture","path":"fixture.json"}]}"#,
            )
            .expect("adapter index");
            std::fs::write(
                adapters.join("fixture.json"),
                r#"{"schema":"homeboy-extension/dependency-adapter-manifest/v1","id":"fixture","version":1,"ecosystem":"fixture","project_signals":{"root_files":["project.json"]},"package_managers":[{"id":"fixture","selection":{"priority":1,"files":["workspace.lock"],"search":"upward"},"commands":{"install":{"command":"mkdir -p deps && printf installed > deps/installed"}},"outputs":[{"path":"deps","kind":"directory"}]}]}"#,
            )
            .expect("adapter manifest");
            let temp = tempfile::tempdir().expect("repository");
            git_output(temp.path(), &["init", "-b", "main"]).expect("init");
            git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
            git_output(temp.path(), &["config", "user.email", "test@example.test"]).expect("email");
            let tool = temp.path().join("tool");
            std::fs::create_dir(&tool).expect("tool");
            std::fs::write(tool.join("project.json"), "{}").expect("project signal");
            std::fs::write(tool.join("workspace.lock"), "").expect("lockfile");
            std::fs::write(temp.path().join(".gitignore"), "deps/\n").expect("ignore");
            git_output(temp.path(), &["add", "."]).expect("add");
            git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
            let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");
            // The candidate workspace was hydrated before its gate ran.
            std::fs::create_dir(tool.join("deps")).expect("deps");
            std::fs::write(tool.join("deps/installed"), "installed").expect("installed");

            let command = "cd tool && cat deps/installed && false";
            let candidate = std::process::Command::new("sh")
                .args(&crate::agent_task_gate::legacy_gate_argv(command)[1..])
                .current_dir(temp.path())
                .output()
                .expect("run candidate gate");
            assert!(!candidate.status.success());
            let mut promotion: AgentTaskPromotionReport =
                serde_json::from_value(serde_json::json!({
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "gate_failed",
                    "source": {"kind": "aggregate", "task_id": "task"},
                    "to_worktree": "fixture",
                    "target": {"worktree": "fixture", "path": temp.path()},
                    "candidate": {"base_sha": "base", "candidate_sha": "patch", "tree_sha": "tree"},
                    "deterministic_gates": [],
                    "operator_notification": {"status": "blocked", "message": "red"}
                }))
                .expect("promotion");
            let stdout = String::from_utf8_lossy(&candidate.stdout).to_string();
            let stderr = String::from_utf8_lossy(&candidate.stderr).to_string();
            let exit_code = candidate.status.code().unwrap_or(1);
            promotion
                .deterministic_gates
                .push(crate::agent_task_gate::AgentTaskGateReport::new(
                    "nested-root-gate",
                    crate::agent_task_gate::legacy_gate_argv(command),
                    exit_code,
                    &stdout,
                    &stderr,
                    None,
                    HomeboyGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                    AgentTaskGateEnvironment::default(),
                ));

            compare_gate_failures_to_verified_base(
                &ObservationStore::open_initialized_at(temp.path().join("observations.sqlite"))
                    .expect("store"),
                &mut promotion,
                &[],
                temp.path(),
                temp.path(),
                &base,
                std::time::Duration::from_secs(30),
                None,
                |_compared, _total| Ok(()),
            )
            .expect("baseline comparison");

            let comparison = promotion.deterministic_gates[0]
                .baseline_comparison
                .as_ref()
                .expect("comparison");
            assert_eq!(
                comparison.result,
                AgentTaskGateDifferentialResult::BaselineRed,
                "{comparison:#?}"
            );
            assert!(comparison.matches_candidate_failure, "{comparison:#?}");
        });
    }

    #[test]
    fn differential_gate_comparison_replays_a_declared_plan_without_shell_projection() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("repository");
            git_output(temp.path(), &["init", "-b", "main"]).expect("init");
            git_output(temp.path(), &["config", "user.name", "Homeboy Test"]).expect("name");
            git_output(temp.path(), &["config", "user.email", "test@example.test"]).expect("email");
            std::fs::write(temp.path().join("marker"), "base\n").expect("base marker");
            git_output(temp.path(), &["add", "marker"]).expect("add");
            git_output(temp.path(), &["commit", "-m", "base"]).expect("commit");
            let base = git_output(temp.path(), &["rev-parse", "HEAD"]).expect("base sha");
            std::fs::write(temp.path().join("marker"), "candidate\n").expect("candidate marker");

            let bin = temp.path().join("bin");
            std::fs::create_dir(&bin).expect("bin");
            let homeboy = bin.join("homeboy");
            homeboy_test_support::write_executable(&homeboy, "#!/bin/sh\ntest \"$1\" = review && test \"$2\" = test || exit 97\n/bin/cat marker >&2\nexit 1\n", 0o755)
        .expect("homeboy adapter");

            let plan = homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec!["homeboy".to_string(), "review".to_string(), "test".to_string()],
            1,
        )
        .expect("plan");
            let mut promotion: AgentTaskPromotionReport =
                serde_json::from_value(serde_json::json!({
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "gate_failed",
                    "source": {"kind": "aggregate", "task_id": "task"},
                    "to_worktree": "fixture",
                    "target": {"worktree": "fixture", "path": temp.path()},
                    "candidate": {"base_sha": "base", "candidate_sha": "patch", "tree_sha": "tree"},
                    "deterministic_gates": [],
                    "operator_notification": {"status": "blocked", "message": "red"}
                }))
                .expect("promotion");
            let mut gate = crate::agent_task_gate::AgentTaskGateReport::new(
                "declared",
                vec![
                    "homeboy".to_string(),
                    "review".to_string(),
                    "test".to_string(),
                ],
                1,
                "",
                "candidate\n",
                None,
                HomeboyGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                crate::agent_task_gate::AgentTaskGateEnvironment {
                    inherited: vec![crate::agent_task_gate::AgentTaskGateEnvironmentVariable {
                        name: "PATH".to_string(),
                        value: bin.display().to_string(),
                    }],
                    ..Default::default()
                },
            );
            gate.invocation = Some(AgentTaskGateInvocation::DeclaredTest { plan: plan.clone() });
            gate.test_execution_outcome =
                Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Failed);
            promotion.deterministic_gates.push(gate);

            compare_gate_failures_to_verified_base(
                &ObservationStore::open_initialized_at(temp.path().join("observations.sqlite"))
                    .expect("store"),
                &mut promotion,
                &[],
                temp.path(),
                temp.path(),
                &base,
                std::time::Duration::from_secs(5),
                None,
                |_compared, _total| Ok(()),
            )
            .expect("typed baseline replay");

            let gate = &promotion.deterministic_gates[0];
            assert_eq!(
                gate.invocation().expect("typed invocation"),
                AgentTaskGateInvocation::DeclaredTest { plan }
            );
            assert_eq!(
                gate.baseline_comparison.as_ref().unwrap().result,
                AgentTaskGateDifferentialResult::BaselineRed
            );
        });
    }
}
