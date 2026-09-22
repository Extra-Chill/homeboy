//! Agent-owned fanout resume policy and effects.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::agent_task_batch as batch;
use crate::agent_task_dependency_actions::{
    execute_resolved_dependency_actions, DependencyAction, DependencyActionExecutor,
    DependencyResolution,
};
use crate::agent_task_fanout_supervisor as supervisor;
use crate::agent_task_lifecycle;
use crate::agent_task_service;
use crate::orchestration::{
    FanoutBatchResumeActionResult, FanoutResumeDispatcherFactory,
    FANOUT_CHILD_RESUME_PARAMETERS_SCHEMA,
};
use homeboy_core::{Error, Result};

#[derive(Clone)]
pub struct FanoutResumeTransport {
    pub executor: crate::agent_task_scheduler::SharedAgentTaskExecutor,
    pub dispatcher: FanoutResumeDispatcherFactory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IssueRef {
    url: String,
    owner: String,
    repo: String,
    number: String,
    key: String,
}
impl IssueRef {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim();
        let Some((prefix, number_part)) = trimmed.split_once("/issues/") else {
            return Err(invalid_fanout(
                "cook-batch issue inputs must be GitHub issue URLs",
            ));
        };
        let number = number_part
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid_fanout(
                "GitHub issue URL is missing a numeric issue number",
            ));
        }
        let mut segments = prefix.trim_end_matches('/').rsplit('/');
        let repo = segments.next().unwrap_or_default();
        let owner = segments.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() {
            return Err(invalid_fanout(
                "GitHub issue URL must include owner and repo",
            ));
        }
        Ok(Self {
            url: trimmed.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: number.to_string(),
            key: format!("{owner}/{repo}#{number}"),
        })
    }
}
fn invalid_fanout(message: &str) -> Error {
    Error::validation_invalid_argument("input", message.to_string(), None, None)
}

pub fn resume_effects(
    batch_id: &str,
    result: &FanoutBatchResumeActionResult,
    transport: &FanoutResumeTransport,
) -> Result<Value> {
    reconcile_fanout_pr_states(batch_id, true)?;
    let batch = batch::read_batch_record(batch_id)?;
    let portfolio = run_portfolio(&batch, transport)?;
    reconcile_fanout_pr_states(batch_id, true)?;
    finalize_resumed_native_worktrees(batch_id, Some(result))?;
    Ok(serde_json::to_value(portfolio)?)
}

/// GitHub is the authority for whether a review-ready candidate was accepted.
/// Persist that observation before asking the durable graph for its executable
/// frontier, so a merge releases only its newly-ready descendants.
pub fn reconcile_fanout_pr_states(
    batch_id: &str,
    mutate: bool,
) -> Result<BTreeMap<String, String>> {
    let batch = batch::read_batch_record(batch_id)?;
    let mut resolutions = Vec::new();
    let mut statuses = BTreeMap::new();
    for child in batch.child_runs {
        let record = if mutate {
            agent_task_lifecycle::reconcile_status(&child.run_id)?
        } else {
            match agent_task_lifecycle::status(&child.run_id) {
                Ok(record) => record,
                // The batch report retains the last durable child state and
                // marks observation freshness separately below, so a read
                // must never fail the whole status on one child's record.
                // That record can be transiently unreadable (a projection
                // lock) or not exist yet at all: children beyond the
                // coordinator's concurrency limit have no durable run record
                // until a worker claims them, even while the batch itself has
                // already left `admitting` for `running` (#14677). Either way
                // there is nothing to reconcile for this child yet.
                Err(_) => continue,
            }
        };
        let Some(mut finalization) = record.metadata.get("cook_finalization").cloned() else {
            continue;
        };
        let pr_ref = finalization
            .get("pr_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                finalization
                    .get("pr_number")
                    .and_then(Value::as_u64)
                    .map(|number| number.to_string())
            })
            // Older durable finalization records used a nested PR reference.
            .or_else(|| {
                finalization
                    .get("pr")
                    .and_then(|pr| pr.get("url"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                finalization
                    .get("pr")
                    .and_then(|pr| pr.get("number"))
                    .and_then(Value::as_u64)
                    .map(|number| number.to_string())
            });
        let Some(pr_ref) = pr_ref else { continue };
        let observation = observe_pr_state(&pr_ref)?;
        let status = observation.verdict();
        let mut pr_observation = observation.as_value();
        if let Some(candidate_revision) = finalization
            .pointer("/publication_proof/binding/candidate_sha")
            .and_then(Value::as_str)
        {
            // The PR head is the precise candidate that was reviewed. Retain
            // the comparison with the merge observation so a changed upstream
            // revision always re-runs dependent invalidation/review instead of
            // trusting the prior candidate's gates.
            pr_observation["candidate_revision"] = Value::String(candidate_revision.to_string());
            pr_observation["candidate_revision_matches"] = Value::Bool(
                observation
                    .head_ref_oid
                    .as_deref()
                    .is_none_or(|head| head == candidate_revision),
            );
        }
        statuses.insert(child.task_id.clone(), status.to_string());
        let transition = match status {
            // An approved candidate makes the next stack level reviewable now.
            // Bind it to the exact head observed so a later force-push/new commit
            // is a distinct durable rebase, gate, and review invalidation.
            "review_ready" => observation
                .head_ref_oid
                .clone()
                .zip(observation.head_ref_name.clone()),
            // Once merged, move the dependent from the candidate branch back to
            // the PR's resolved target branch using the merge commit.
            "merged" => observation
                .merge_commit
                .as_ref()
                .map(|commit| commit.oid.clone())
                .zip(observation.base_ref_name.clone()),
            _ => None,
        };
        if let Some((upstream_revision, target_base)) = transition {
            resolutions.push(DependencyResolution {
                child_id: child.task_id.clone(),
                upstream_revision,
                target_base,
            });
        }
        if !mutate
            || (finalization.get("status").and_then(Value::as_str) == Some(status)
                && finalization.get("pr_observation") == Some(&pr_observation))
        {
            continue;
        }
        finalization["status"] = Value::String(status.to_string());
        finalization["pr_observation"] = pr_observation;
        agent_task_lifecycle::record_cook_finalization(&child.run_id, finalization)?;
    }
    if mutate && !resolutions.is_empty() {
        execute_resolved_dependency_actions(
            batch_id,
            &resolutions,
            &mut LocalDependencyActionExecutor,
        )?;
    }
    Ok(statuses)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanoutPrObservation {
    pub state: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default)]
    pub merge_state_status: Option<String>,
    #[serde(default)]
    pub merge_commit: Option<FanoutMergeCommit>,
    #[serde(default)]
    pub base_ref_name: Option<String>,
    #[serde(default)]
    pub head_ref_oid: Option<String>,
    #[serde(default)]
    pub head_ref_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FanoutMergeCommit {
    pub oid: String,
}

impl FanoutPrObservation {
    pub fn verdict(&self) -> &'static str {
        match self.state.as_str() {
            "MERGED" | "CLOSED" if self.merged_at.is_some() => "merged",
            "CLOSED" => "rejected",
            "OPEN" if self.review_decision.as_deref() == Some("CHANGES_REQUESTED") => {
                "revision_requested"
            }
            _ => "review_ready",
        }
    }

    fn as_value(&self) -> Value {
        serde_json::json!({
            "state": self.state,
            "merged_at": self.merged_at,
            "review_decision": self.review_decision,
            "merge_state_status": self.merge_state_status,
            "merge_commit_oid": self.merge_commit.as_ref().map(|commit| &commit.oid),
            "base_ref_name": self.base_ref_name,
            "head_ref_oid": self.head_ref_oid,
            "head_ref_name": self.head_ref_name,
        })
    }
}

fn observe_pr_state(pr: &str) -> Result<FanoutPrObservation> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            pr,
            "--json",
            "state,mergedAt,reviewDecision,mergeStateStatus,mergeCommit,baseRefName,headRefOid,headRefName",
        ])
        .output()
        .map_err(|error| Error::git_command_failed(format!("gh pr view {pr}: {error}")))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "gh pr view {pr}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::internal_json(format!("parse gh pr view {pr}: {error}"), None))
}

struct LocalDependencyActionExecutor;

impl DependencyActionExecutor for LocalDependencyActionExecutor {
    fn side_effect_applied(&mut self, action: &DependencyAction, step: &str) -> Result<bool> {
        match step {
            "fetch" => run_dependency_command(
                &action.worktree,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", action.upstream_revision),
                ],
            )
            .map(|()| true)
            .or_else(|_| Ok(false)),
            "rebase" => Command::new("git")
                .args([
                    "merge-base",
                    "--is-ancestor",
                    &action.upstream_revision,
                    "HEAD",
                ])
                .current_dir(&action.worktree)
                .status()
                .map(|status| status.success())
                .map_err(|error| Error::git_command_failed(format!("git merge-base: {error}"))),
            "push" => {
                let local = dependency_command_output(&action.worktree, &["rev-parse", "HEAD"])?;
                let remote = dependency_command_output(
                    &action.worktree,
                    &[
                        "ls-remote",
                        "--heads",
                        "origin",
                        &format!("refs/heads/{}", action.head),
                    ],
                )?;
                Ok(remote
                    .split_whitespace()
                    .next()
                    .is_some_and(|revision| revision == local))
            }
            "pull_request_base_update" => {
                let Some(pr) = action.pull_request.as_deref() else {
                    return Ok(true);
                };
                let observation = observe_pr_state(pr)?;
                Ok(observation.base_ref_name.as_deref() == Some(&action.target_base))
            }
            // These are durable-local transitions, not GitHub/Git side effects.
            _ => Ok(false),
        }
    }

    fn fetch(&mut self, action: &DependencyAction) -> Result<()> {
        homeboy_core::git::fetch_remote_tracking_refs_until(
            Path::new(&action.worktree),
            &["fetch", "--no-tags", "origin", &action.upstream_revision],
            "git fetch dependency upstream revision",
            &[],
            Instant::now() + Duration::from_secs(30),
        )
        .map(|_| ())
    }

    fn rebase(&mut self, action: &DependencyAction) -> Result<()> {
        run_dependency_command(&action.worktree, &["rebase", &action.upstream_revision])
    }

    fn push(&mut self, action: &DependencyAction) -> Result<()> {
        run_dependency_command(
            &action.worktree,
            &[
                "push",
                "--force-with-lease",
                "origin",
                &format!("HEAD:{}", action.head),
            ],
        )
    }

    fn update_pull_request_base(&mut self, action: &DependencyAction) -> Result<()> {
        let Some(pr) = action.pull_request.as_deref() else {
            return Ok(());
        };
        let output = Command::new("gh")
            .args(["pr", "edit", pr, "--base", &action.target_base])
            .current_dir(&action.worktree)
            .output()
            .map_err(|error| Error::git_command_failed(format!("gh pr edit {pr}: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::git_command_failed(format!(
                "gh pr edit {pr}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    fn invalidate_review(&mut self, action: &DependencyAction) -> Result<()> {
        // The Cook lifecycle has already been re-armed by the preceding durable
        // gate-invalidation step. Keep review invalidation as its own receipt.
        let _ = action;
        Ok(())
    }
}

fn run_dependency_command(path: &str, arguments: &[&str]) -> Result<()> {
    dependency_command_output(path, arguments).map(|_| ())
}

fn dependency_command_output(path: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .map_err(|error| {
            Error::git_command_failed(format!("git {}: {error}", arguments.join(" ")))
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(Error::git_command_failed(format!(
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Mutating supervisor entrypoint. Constructing the production adapter here
/// keeps Cook's durable continuation, force-with-lease receipt, and PR recovery
/// on the public `fanout resume` path rather than a status projection.
fn run_portfolio(
    batch_record: &crate::agent_tasks::AgentTaskBatchRecord,
    transport: &FanoutResumeTransport,
) -> Result<supervisor::AgentTaskFanoutPortfolioRunReport> {
    let mut portfolio = load_portfolio(batch_record)?;
    let dependencies = durable_graph_dependencies(batch_record)?;
    portfolio.run(
        &mut CookFanoutPortfolioAdapter {
            transport: transport.clone(),
        },
        &dependencies,
    )
}

fn load_portfolio(
    batch_record: &crate::agent_tasks::AgentTaskBatchRecord,
) -> Result<supervisor::AgentTaskFanoutPortfolio> {
    match supervisor::read_portfolio(&batch_record.batch_id) {
        Ok(mut portfolio) => {
            for child in &batch_record.child_runs {
                if let Some(portfolio_child) = portfolio.children.get_mut(&child.task_id) {
                    portfolio_child.run_id.clone_from(&child.run_id);
                }
            }
            Ok(portfolio)
        }
        Err(_) if !supervisor::portfolio_exists(&batch_record.batch_id)? => {
            Ok(supervisor::AgentTaskFanoutPortfolio::new(
                batch_record.batch_id.clone(),
                batch_record.child_runs.iter().map(|child| {
                    supervisor::AgentTaskFanoutPortfolioChild {
                        child_id: child.task_id.clone(),
                        tracker_ref: agent_task_lifecycle::status(&child.run_id)
                            .ok()
                            .and_then(|record| {
                                declared_tracker_ref(&record.metadata).map(str::to_string)
                            })
                            .or_else(|| {
                                batch_record.metadata["declared_trackers"][&child.task_id]
                                    .as_str()
                                    .map(str::to_string)
                            })
                            .unwrap_or_else(|| {
                                format!("homeboy://agent-task/run/{}", child.run_id)
                            }),
                        run_id: child.run_id.clone(),
                        source_sha: None,
                        base_sha: None,
                        head_sha: None,
                        evidence_generation: 0,
                        finding_fingerprints: Default::default(),
                        finding_fingerprint_recency: Default::default(),
                        blocker: None,
                        next_action: None,
                    }
                }),
            ))
        }
        Err(error) => Err(error),
    }
}

/// Consume the graph owner's typed readiness projection without duplicating its
/// topology, state, or downstream action contracts.
pub(crate) struct DurableGraphDependencies {
    batch_id: String,
    readiness: Option<crate::agent_tasks::dependency_graph::AgentTaskDependencyReadiness>,
}

impl supervisor::FanoutDependencyResolver for DurableGraphDependencies {
    fn readiness(&self, child_id: &str) -> supervisor::FanoutDependencyReadiness {
        use crate::agent_tasks::dependency_graph::AgentTaskDependencyState;

        let Some(readiness) = &self.readiness else {
            return supervisor::FanoutDependencyReadiness::Ready;
        };
        if readiness.states.get(child_id) == Some(&AgentTaskDependencyState::Ready) {
            return supervisor::FanoutDependencyReadiness::Ready;
        }
        let detail = readiness
            .blocked_paths
            .get(child_id)
            .map(|path| path.join(" <- "))
            .unwrap_or_else(|| {
                let state = readiness.states.get(child_id).copied();
                format!("dependency graph projects child state '{state:?}'")
            });
        supervisor::FanoutDependencyReadiness::Blocked {
            detail,
            evidence_ref: format!(
                "homeboy://agent-task/batch/{}/dependency-graph/children/{child_id}",
                self.batch_id
            ),
        }
    }
}

pub(crate) fn durable_graph_dependencies(
    batch_record: &crate::agent_tasks::AgentTaskBatchRecord,
) -> Result<DurableGraphDependencies> {
    let Some(graph) = batch_record.metadata.get("dependency_graph") else {
        return Ok(DurableGraphDependencies {
            batch_id: batch_record.batch_id.clone(),
            readiness: None,
        });
    };
    let readiness = graph
        .get("readiness")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(DurableGraphDependencies {
        batch_id: batch_record.batch_id.clone(),
        readiness,
    })
}

/// Production adapter for the durable child action executor. Cook owns the
/// provider, promotion, gate, review, Git, and PR contracts; this adapter only
/// chooses the child-local, idempotent continuation that must run next.
struct CookFanoutPortfolioAdapter {
    transport: FanoutResumeTransport,
}

impl supervisor::FanoutPortfolioAdapter for CookFanoutPortfolioAdapter {
    fn observe(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<supervisor::AgentTaskFanoutPortfolioObservation> {
        portfolio_observation(
            &child.child_id,
            &child.run_id,
            true,
            Some(&child.tracker_ref),
        )
    }

    fn continue_provider(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(&self.transport, child, false)
    }

    fn rebase_candidate(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(&self.transport, child, true)
    }

    fn recreate_candidate(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        // Recreate is intentionally a separate continuation request. The Cook
        // recovery contract selects only its persisted recreation path.
        resume_fanout_child(&self.transport, child, true)
    }

    fn rerun_gates_and_review(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(&self.transport, child, true)
    }

    fn finalize_or_update_pr(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
        should_force_with_lease: bool,
    ) -> Result<()> {
        if should_force_with_lease {
            force_with_lease_then_reconcile(child)
        } else {
            resume_fanout_child(&self.transport, child, false)
        }
    }
}

/// Publish the already-gated candidate with a remote compare-and-swap, persist
/// its receipt, then ask Cook's recovery finalizer to refresh the existing PR.
/// This deliberately bypasses Cook's cached-finalization return path.
fn force_with_lease_then_reconcile(
    child: &supervisor::AgentTaskFanoutPortfolioChild,
) -> Result<()> {
    let record = agent_task_lifecycle::reconcile_status(&child.run_id)?;
    let promotion = record.metadata.get("latest_promotion").ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "force-with-lease requires the durable promoted candidate",
            Some(child.run_id.clone()),
            None,
        )
    })?;
    let path = promotion
        .pointer("/provenance/worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "force-with-lease requires the promoted candidate worktree",
                Some(child.run_id.clone()),
                None,
            )
        })?;
    let finalization = record.metadata.get("cook_finalization").ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_finalization",
            "force-with-lease requires a prior Cook finalization with its PR head branch",
            Some(child.run_id.clone()),
            None,
        )
    })?;
    let head = finalization
        .get("head")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_finalization.head",
                "force-with-lease requires the prior finalization head branch",
                Some(child.run_id.clone()),
                None,
            )
        })?;
    // One store for both receipt writes and the status read below: the two
    // receipts describe one force-with-lease and must land in one home.
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let receipt = force_with_lease_push(path, head)?;
    agent_task_lifecycle::record_cook_force_with_lease_receipt_in_store(
        &lifecycle_store,
        &child.run_id,
        receipt.clone(),
    )?;
    agent_task_service::recover_cook_pr(&child.run_id, Vec::new(), false)?;
    let mut receipt = receipt;
    receipt["pr_refresh_completed"] = Value::Bool(true);
    agent_task_lifecycle::record_cook_force_with_lease_receipt_in_store(
        &lifecycle_store,
        &child.run_id,
        receipt,
    )?;
    Ok(())
}

/// Compare-and-swap the already-gated candidate onto its PR branch. Keeping
/// this boundary independent of lifecycle mutation makes the expected remote
/// SHA, command, and post-push observation directly verifiable.
pub fn force_with_lease_push(path: &str, head: &str) -> Result<Value> {
    let destination = format!("refs/heads/{head}");
    let expected_sha = git_stdout(path, &["ls-remote", "--heads", "origin", &destination])?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "cannot force-with-lease a missing remote branch `{destination}`"
            ))
        })?;
    let candidate_sha = git_stdout(path, &["rev-parse", "HEAD"])?;
    let lease = format!("--force-with-lease={destination}:{expected_sha}");
    let refspec = format!("{candidate_sha}:{destination}");
    // A restart may observe the completed push before its receipt was durable.
    // The matching remote ref is sufficient to record that receipt and refresh
    // the PR; issuing a second force-push would widen the interruption window.
    if expected_sha != candidate_sha {
        git_stdout(path, &["push", &lease, "origin", &refspec])?;
    }
    let after_sha = git_stdout(path, &["ls-remote", "--heads", "origin", &destination])?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "force-with-lease did not leave remote branch `{destination}` readable"
            ))
        })?;
    if after_sha != candidate_sha {
        return Err(Error::git_command_failed(format!(
            "force-with-lease left `{destination}` at `{after_sha}` instead of candidate `{candidate_sha}`"
        )));
    }
    Ok(serde_json::json!({
        "command": ["git", "push", lease, "origin", refspec],
        "remote": "origin",
        "ref": destination,
        "expected_sha": expected_sha,
        "after_sha": after_sha,
        "reconciled_existing_push": expected_sha == candidate_sha,
        // The receipt is intentionally incomplete until the PR host has been
        // refreshed. A restart then resumes refresh without repeating a push.
        "pr_refresh_completed": false,
    }))
}

fn git_stdout(path: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resume_fanout_child(
    transport: &FanoutResumeTransport,
    child: &supervisor::AgentTaskFanoutPortfolioChild,
    rerun_completed_gates: bool,
) -> Result<()> {
    let record = agent_task_lifecycle::status(&child.run_id)?;
    let generation = record
        .updated_at
        .clone()
        .unwrap_or_else(|| record.submitted_at.clone());
    let request = homeboy_control_plane_contract::ControlPlaneActionRequest {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
        effect_id: homeboy_control_plane_contract::action_effect_id(
            "fanout-child",
            &child.run_id,
            "resume",
            &generation,
        ),
        action: homeboy_control_plane_contract::ControlPlaneAction::Resume,
        idempotency_key: format!("fanout-child-resume:{}:{}", child.run_id, generation),
        actor: "agent-task-fanout-supervisor".to_string(),
        expected_updated_at: Some(generation),
        parameters: homeboy_control_plane_contract::ControlPlaneActionPayload {
            schema: FANOUT_CHILD_RESUME_PARAMETERS_SCHEMA.to_string(),
            data: serde_json::json!({
                "rerun_completed_gates": rerun_completed_gates,
                "invalidated_action_payload": {
                    "gates": { "rerun_completed_gates": rerun_completed_gates }
                }
            }),
        },
        confirmed: true,
    };
    let acknowledgement = crate::orchestration::execute_fanout_child_resume_action(
        &child.run_id,
        &request,
        transport.executor.clone(),
        transport.dispatcher,
    )?;
    if acknowledgement.outcome == homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed
    {
        return Err(Error::internal_unexpected(
            acknowledgement
                .message
                .unwrap_or_else(|| "fanout child resume action failed".to_string()),
        ));
    }
    Ok(())
}

pub fn portfolio_observation(
    child_id: &str,
    run_id: &str,
    reconcile: bool,
    declared_tracker: Option<&str>,
) -> Result<crate::agent_tasks::fanout_supervisor::AgentTaskFanoutPortfolioObservation> {
    use crate::agent_tasks::fanout_supervisor as supervisor;
    let record = if reconcile {
        agent_task_lifecycle::reconcile_status(run_id)
    } else {
        agent_task_lifecycle::status(run_id)
    }
    .ok();
    let provider = match record.as_ref().map(|record| record.state) {
        Some(agent_task_lifecycle::AgentTaskRunState::Running) => {
            supervisor::AgentTaskFanoutProviderState::Running
        }
        Some(
            agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable,
        ) => supervisor::AgentTaskFanoutProviderState::Succeeded,
        Some(_) => supervisor::AgentTaskFanoutProviderState::Failed,
        None => supervisor::AgentTaskFanoutProviderState::Pending,
    };
    // Provider interruption can occur before promotion writes its provenance.
    // The recipe is already durable at provider dispatch, so retain its declared
    // worktree and tracker identity for the recovery projection.
    let recipe = agent_task_service::load_recipe_for_attempt(run_id)
        .ok()
        .flatten();
    let promotion = record
        .as_ref()
        .and_then(|record| record.metadata.get("latest_promotion"));
    let path = promotion
        .and_then(|value| value.pointer("/provenance/worktree_path"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            // `to_worktree` on the recipe is a workspace *handle*
            // (`component@branch`), not a filesystem path. Treating it as a
            // path here always failed `git status`, reporting a worktree
            // that was still durably registered and present on disk as
            // `missing` whenever the promotion checkpoint had not been
            // written yet or could not be read (#14734, #14735). Resolve the
            // handle through the worktree registry so the real path backs
            // the observation instead.
            let handle = recipe
                .as_ref()
                .and_then(|recipe| recipe.finalization.get("to_worktree"))
                .and_then(Value::as_str)?;
            // `resolve_worktree_ownership_if_present` also gates on reuse
            // safety (it rejects a dirty worktree), which is the wrong
            // question for a read-only observation: an uncommitted candidate
            // is exactly the state this projection needs to surface, not
            // reject. Read the raw registered record instead.
            homeboy_core::worktree::resolve_workspace_ref_if_present(handle)
                .ok()
                .flatten()
                .filter(|record| {
                    *record.state() != homeboy_core::worktree::TaskWorktreeState::Removed
                })
                .map(|record| record.path().to_string())
        });
    let declared_base = promotion
        .and_then(|value| value.pointer("/verified_base/base"))
        .and_then(Value::as_str);
    let (worktree, head_sha, current_base_sha) = path
        .as_deref()
        .map(|path| git_candidate_state(path, declared_base))
        .unwrap_or((
            supervisor::AgentTaskFanoutWorktreeState::Missing,
            None,
            None,
        ));
    let base_sha = promotion
        .and_then(|value| value.pointer("/verified_base/sha"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let source_sha = promotion
        .and_then(|value| value.pointer("/provenance/source_sha"))
        .or_else(|| promotion.and_then(|value| value.pointer("/source/sha")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let gates = match promotion
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
    {
        // `applied`/`verified_no_changes` are the real terminal promotion
        // status strings for a passing gate phase; `gate_failed`/
        // `no_changes_gate_failed` are the real strings for a failing one.
        // The literal `"failed"` this matched against before never occurs,
        // so a gate-failed candidate was silently reported as `missing`
        // evidence instead of `failed` (#14735).
        Some("applied" | "verified_no_changes") => {
            supervisor::AgentTaskFanoutEvidenceState::Current
        }
        Some("gate_failed" | "no_changes_gate_failed") => {
            supervisor::AgentTaskFanoutEvidenceState::Failed
        }
        _ => supervisor::AgentTaskFanoutEvidenceState::Missing,
    };
    let finalization = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_finalization"));
    let accepted_evidence = finalization
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        == Some("review_ready");
    let receipt_current = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_force_with_lease_receipt"))
        .and_then(|receipt| {
            (receipt.get("pr_refresh_completed") == Some(&Value::Bool(true))).then_some(receipt)
        })
        .and_then(|receipt| receipt.get("after_sha"))
        .and_then(Value::as_str)
        .is_some_and(|after_sha| head_sha.as_deref() == Some(after_sha));
    let (tracker, pr, remote_head_sha, findings) = match path.as_deref() {
        Some(path) => github_observation(path, record.as_ref(), finalization, declared_tracker)?,
        None => (
            tracker_state_without_observation(record.as_ref(), recipe.as_ref(), declared_tracker),
            supervisor::AgentTaskFanoutPrState::Unknown,
            None,
            Vec::new(),
        ),
    };
    Ok(supervisor::AgentTaskFanoutPortfolioObservation {
        child_id: child_id.to_string(),
        tracker,
        provider,
        worktree,
        candidate: supervisor::AgentTaskFanoutCandidateState {
            source_sha,
            base_sha,
            head_sha,
            current_base_sha,
            remote_head_sha,
            publication_receipt_current: receipt_current,
            can_rebase: path.is_some(),
            can_recreate: false,
        },
        gates,
        // A durable finalization record is the accepted local evidence. Host
        // review decisions and findings below are independently refreshed.
        acceptance: if accepted_evidence {
            supervisor::AgentTaskFanoutEvidenceState::Current
        } else {
            supervisor::AgentTaskFanoutEvidenceState::Missing
        },
        pr,
        findings,
    })
}

/// Read tracker and PR state through Homeboy's existing GitHub API boundary.
/// Keeping this at the CLI composition layer makes the supervisor itself
/// injectable and product-neutral while avoiding synthetic "open" states.
pub fn github_observation(
    path: &str,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    finalization: Option<&Value>,
    declared_tracker: Option<&str>,
) -> Result<(
    supervisor::AgentTaskFanoutTrackerState,
    supervisor::AgentTaskFanoutPrState,
    Option<String>,
    Vec<supervisor::AgentTaskFanoutReviewFinding>,
)> {
    let tracker = match record
        .and_then(|record| declared_tracker_ref(&record.metadata))
        .or(declared_tracker.filter(|reference| is_tracker_url(reference)))
    {
        Some(task_url) => match IssueRef::parse(task_url) {
            Ok(issue) => match homeboy_core::git::issue_find(
                None,
                homeboy_core::git::IssueFindOptions {
                    state: homeboy_core::git::IssueState::All,
                    limit: 100,
                    path: Some(path.to_string()),
                    ..Default::default()
                },
            ) {
                Ok(result) => result
                    .items
                    .iter()
                    .find(|item| item.number.to_string() == issue.number)
                    .map_or(
                        supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved,
                        |item| {
                            if item.state.eq_ignore_ascii_case("open") {
                                supervisor::AgentTaskFanoutTrackerState::Open
                            } else {
                                supervisor::AgentTaskFanoutTrackerState::Closed
                            }
                        },
                    ),
                // A declared tracker is still durable evidence when the host
                // cannot be observed from a recovered provider worktree.
                Err(_) => supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved,
            },
            // Tracker identity is generic; this adapter only observes GitHub.
            Err(_) => supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved,
        },
        None => supervisor::AgentTaskFanoutTrackerState::Unknown,
    };
    let head = finalization
        .and_then(|value| value.get("head"))
        .and_then(Value::as_str);
    let base = finalization
        .and_then(|value| value.get("base"))
        .and_then(Value::as_str);
    let Some(head) = head else {
        return Ok((
            tracker,
            supervisor::AgentTaskFanoutPrState::Missing,
            None,
            Vec::new(),
        ));
    };
    let prs = homeboy_core::git::pr_find(
        None,
        homeboy_core::git::PrFindOptions {
            head: Some(head.to_string()),
            base: base.map(str::to_string),
            state: homeboy_core::git::PrState::All,
            limit: 10,
            path: Some(path.to_string()),
        },
    )?;
    let Some(pr) = prs.items.first() else {
        return Ok((
            tracker,
            supervisor::AgentTaskFanoutPrState::Missing,
            None,
            Vec::new(),
        ));
    };
    let view = homeboy_core::git::pr_view(None, pr.number, Some(path.to_string()))?;
    let findings = matches!(view.review_decision.as_deref(), Some("CHANGES_REQUESTED"))
        .then(|| supervisor::AgentTaskFanoutReviewFinding {
            fingerprint: format!("github-pr-{}-changes-requested", view.number),
            summary: "GitHub review decision is changes requested".to_string(),
        })
        .into_iter()
        .collect();
    let state = if view.merged_at.is_some() {
        supervisor::AgentTaskFanoutPrState::Merged
    } else if view.ci_state.eq_ignore_ascii_case("terminal_green") {
        supervisor::AgentTaskFanoutPrState::OpenChecksPassing
    } else if view.ci_state.eq_ignore_ascii_case("failure") {
        supervisor::AgentTaskFanoutPrState::OpenChecksFailed
    } else {
        supervisor::AgentTaskFanoutPrState::OpenChecksPending
    };
    Ok((tracker, state, view.head_sha, findings))
}

pub fn declared_tracker_ref(metadata: &Value) -> Option<&str> {
    metadata
        .pointer("/cook_recipe/source_refs/0")
        .and_then(Value::as_str)
        .filter(|reference| is_tracker_url(reference))
}

fn is_tracker_url(reference: &str) -> bool {
    reference.starts_with("https://") || reference.starts_with("http://")
}

fn tracker_state_without_observation(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    recipe: Option<&crate::agent_task_service::AgentTaskCookRecipe>,
    declared_tracker: Option<&str>,
) -> supervisor::AgentTaskFanoutTrackerState {
    if record.is_some_and(|record| declared_tracker_ref(&record.metadata).is_some())
        || recipe.is_some_and(|recipe| {
            recipe
                .source_refs
                .first()
                .is_some_and(|reference| is_tracker_url(reference))
        })
        || declared_tracker.is_some_and(is_tracker_url)
    {
        supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved
    } else {
        supervisor::AgentTaskFanoutTrackerState::Unknown
    }
}

fn git_candidate_state(
    path: &str,
    declared_base: Option<&str>,
) -> (
    crate::agent_tasks::fanout_supervisor::AgentTaskFanoutWorktreeState,
    Option<String>,
    Option<String>,
) {
    use crate::agent_tasks::fanout_supervisor::AgentTaskFanoutWorktreeState;
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();
    let Ok(status) = status else {
        return (AgentTaskFanoutWorktreeState::Missing, None, None);
    };
    if !status.status.success() {
        return (AgentTaskFanoutWorktreeState::Missing, None, None);
    }
    let worktree = if status.stdout.is_empty() {
        AgentTaskFanoutWorktreeState::Clean
    } else if String::from_utf8_lossy(&status.stdout)
        .lines()
        .any(|line| line.starts_with("UU") || line.starts_with("AA") || line.starts_with("DD"))
    {
        AgentTaskFanoutWorktreeState::Conflicted
    } else {
        AgentTaskFanoutWorktreeState::Dirty
    };
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    // Observation must be safe for `fanout status`: use the existing
    // remote-tracking ref rather than fetching and mutating the worktree.
    let base = declared_base.and_then(|base| {
        let reference = format!("refs/remotes/origin/{base}");
        Command::new("git")
            .args(["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
            .current_dir(path)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    });
    (worktree, head, base)
}

pub fn native_worktree_disposition(
    status: &str,
    exit_code: i32,
) -> Option<homeboy_core::worktree_provider::WorktreeTerminalDisposition> {
    use homeboy_core::worktree_provider::WorktreeTerminalDisposition as Disposition;
    match status {
        // These successful/recoverable outcomes still own the workspace until
        // later publication or acceptance reaches a provider-terminal state.
        "green_no_finalize" | "awaiting_acceptance" => None,
        "cancelled" => Some(Disposition::Cancelled),
        "timed_out" => Some(Disposition::TimedOut),
        "review_form_timeout" => None,
        "pre_artifact_interruption" => Some(Disposition::Interrupted),
        "review_ready"
        | "draft_published"
        | "completed"
        | "intentional_no_change"
        | "no_candidate"
        | "no_changes"
            if exit_code == 0 =>
        {
            Some(Disposition::Succeeded)
        }
        _ if exit_code == 0 => None,
        _ => Some(Disposition::Failed),
    }
}

fn finalize_resumed_native_worktrees(
    batch_id: &str,
    resumed: Option<&crate::orchestration::FanoutBatchResumeActionResult>,
) -> Result<()> {
    let batch = batch::read_batch_record(batch_id)?;
    for child in &batch.child_runs {
        let result = (|| {
            let live_record = || match agent_task_lifecycle::reconcile_status(&child.run_id) {
                Ok(current) => Ok(Some(current)),
                Err(error) if error.message.contains("agent-task run record not found") => Ok(None),
                Err(error) => Err(error),
            };
            let live = live_record()?;
            // A live record always outranks the resumed report and batch roster.
            // In particular, a live nonterminal child fences stale terminal data.
            if live
                .as_ref()
                .is_some_and(|record| !record.state.is_terminal())
            {
                return Ok(());
            }
            let resumed_cell = resumed.and_then(|report| {
                report
                    .cooks
                    .iter()
                    .find(|cook| cook.initial_run_id == child.run_id && cook.terminal)
            });
            let state = live
                .as_ref()
                .map(|record| record.state)
                .or_else(|| {
                    resumed_cell.map(|cook| {
                        if cook.exit_code == 0 {
                            agent_task_lifecycle::AgentTaskRunState::Succeeded
                        } else {
                            agent_task_lifecycle::AgentTaskRunState::Failed
                        }
                    })
                })
                .or_else(|| child.state.is_terminal().then_some(child.state));
            let Some(state) = state else { return Ok(()) };
            let deferred_status = batch.metadata["provider_worktree_finalization_deferrals"]
                [&child.run_id]["lifecycle_status"]
                .as_str()
                .or_else(|| {
                    // Compatibility for records written before deferral became
                    // orthogonal to the exact provider mutation intent.
                    batch.metadata["provider_worktree_finalizations"][&child.run_id]
                        ["lifecycle_status"]
                        .as_str()
                });
            let live_terminal_status = live.as_ref().and_then(|record| {
                record
                    .metadata
                    .pointer("/cook_progress/terminal_status")
                    .and_then(Value::as_str)
            });
            let status = match live.as_ref() {
                Some(record) => record
                    .metadata
                    .get("cook_finalization")
                    .and_then(|finalization| finalization.get("status"))
                    .and_then(Value::as_str)
                    .or(live_terminal_status),
                None => resumed_cell.map(|cell| cell.status.as_str()),
            };
            let disposition = match state {
                agent_task_lifecycle::AgentTaskRunState::Succeeded => {
                    let Some(disposition) =
                        native_worktree_disposition(status.unwrap_or_default(), 0)
                    else {
                        return Ok(());
                    };
                    disposition
                }
                agent_task_lifecycle::AgentTaskRunState::PartialFailure
                | agent_task_lifecycle::AgentTaskRunState::Failed => {
                    if deferred_status == Some("review_form_timeout")
                        && live_terminal_status.is_none_or(|status| status == "review_form_timeout")
                    {
                        return Ok(());
                    }
                    homeboy_core::worktree_provider::WorktreeTerminalDisposition::Failed
                }
                agent_task_lifecycle::AgentTaskRunState::Cancelled => {
                    homeboy_core::worktree_provider::WorktreeTerminalDisposition::Cancelled
                }
                // Recoverable children still need Cook promotion, gates, and PR
                // finalization before their provider workspace may be released.
                agent_task_lifecycle::AgentTaskRunState::Queued
                | agent_task_lifecycle::AgentTaskRunState::Running
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable => return Ok(()),
            };
            if portfolio_vetoes_success_cleanup(batch_id, &child.run_id, disposition)? {
                batch::record_provider_worktree_finalization_deferred(
                    batch_id,
                    &child.run_id,
                    "portfolio_retention_blocker",
                )?;
                return Ok(());
            }
            let recipe =
                agent_task_service::load_recipe_for_attempt(&child.run_id)?.ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "run_id",
                        "terminal fanout child is missing its durable Cook recipe",
                        Some(child.run_id.clone()),
                        None,
                    )
                })?;
            let owner = recipe
                .attempts
                .first()
                .expect("validated Cook recipe has an initial attempt")
                .run_id
                .clone();
            let handle = recipe
                .finalization
                .get("to_worktree")
                .and_then(Value::as_str)
                .filter(|handle| !handle.trim().is_empty())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "cook_recipe.finalization.to_worktree",
                        "terminal fanout child recipe has no provider worktree handle",
                        Some(recipe.cook_id.clone()),
                        None,
                    )
                })?;
            finalize_fanout_native_worktree(batch_id, &child.run_id, handle, &owner, disposition)
        })();
        if let Err(error) = result {
            let _ = batch::record_provider_worktree_finalization_preflight_error(
                batch_id,
                &child.run_id,
                &error,
            );
        }
    }
    Ok(())
}

pub fn portfolio_vetoes_success_cleanup(
    batch_id: &str,
    child_run_id: &str,
    disposition: homeboy_core::worktree_provider::WorktreeTerminalDisposition,
) -> Result<bool> {
    if disposition != homeboy_core::worktree_provider::WorktreeTerminalDisposition::Succeeded
        || !supervisor::portfolio_exists(batch_id)?
    {
        return Ok(false);
    }
    let portfolio = supervisor::read_portfolio(batch_id)?;
    let batch = batch::read_batch_record(batch_id)?;
    let task_id = batch
        .child_runs
        .iter()
        .find(|child| child.run_id == child_run_id)
        .map(|child| child.task_id.as_str());
    Ok(task_id
        .and_then(|task_id| portfolio.children.get(task_id))
        .or_else(|| {
            portfolio
                .children
                .values()
                .find(|child| child.run_id == child_run_id)
        })
        .is_some_and(|child| {
            child.blocker.is_some()
                || child.next_action.as_ref().is_some_and(|action| {
                    !matches!(action, supervisor::AgentTaskFanoutPortfolioAction::None)
                })
        }))
}

pub fn finalize_fanout_native_worktree(
    batch_id: &str,
    child_run_id: &str,
    handle: &str,
    owner: &str,
    disposition: homeboy_core::worktree_provider::WorktreeTerminalDisposition,
) -> Result<()> {
    let finalization = batch::finalize_provider_worktree_for_child(
        batch_id,
        child_run_id,
        handle,
        &homeboy_core::worktree_provider::WorktreeProvisionLifecycle {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: owner.to_string(),
            cleanup_policy: homeboy_core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess,
        },
        disposition,
    )?;
    match finalization {
        batch::BatchProviderWorktreeFinalization::Finalized
        | batch::BatchProviderWorktreeFinalization::Replayed
        | batch::BatchProviderWorktreeFinalization::Unsupported
        | batch::BatchProviderWorktreeFinalization::NotFound => Ok(()),
    }
}
