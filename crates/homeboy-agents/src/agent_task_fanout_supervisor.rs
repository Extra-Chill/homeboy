//! Durable, dependency-agnostic portfolio reconciliation for fanout children.
//!
//! Dependency readiness deliberately enters through [`FanoutDependencyResolver`].
//! The graph and stacked-base semantics belong to #10946; this supervisor only
//! consumes its answer while reconciling otherwise independent children.

use chrono::Utc;
use homeboy_core::{paths, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

pub const AGENT_TASK_FANOUT_PORTFOLIO_SCHEMA: &str = "homeboy/agent-task-fanout-portfolio/v1";
pub const AGENT_TASK_FANOUT_PORTFOLIO_STATUS_SCHEMA: &str =
    "homeboy/agent-task-fanout-portfolio-status/v1";
/// Keep durable dedupe evidence useful without allowing an unbounded stream of
/// distinct review findings to grow the controller state forever.
pub const PORTFOLIO_FINDING_FINGERPRINT_LIMIT: usize = 128;
pub const CHILD_FINDING_FINGERPRINT_LIMIT: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolio {
    #[serde(default = "portfolio_schema")]
    pub schema: String,
    pub fanout_id: String,
    pub children: BTreeMap<String, AgentTaskFanoutPortfolioChild>,
    #[serde(default)]
    pub finding_fingerprints: BTreeSet<String>,
    #[serde(default)]
    pub finding_fingerprint_recency: BTreeMap<String, u64>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioChild {
    pub child_id: String,
    pub tracker_ref: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub evidence_generation: u64,
    #[serde(default)]
    pub finding_fingerprints: BTreeSet<String>,
    #[serde(default)]
    pub finding_fingerprint_recency: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<AgentTaskFanoutPortfolioBlocker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<AgentTaskFanoutPortfolioAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioBlocker {
    pub code: String,
    pub detail: String,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFanoutPortfolioAction {
    ContinueProvider,
    ResumeFinalization,
    RebaseAndRerunGates,
    RecreateCandidateAndRerunGates,
    RerunGates,
    UpdatePrForceWithLease,
    AwaitAcceptance,
    InspectBlockedCandidate,
    None,
}

/// The observation boundary is intentionally supplied by the runtime adapter.
/// It keeps source control, tracker, PR host, and provider implementations out
/// of the durable portfolio contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioObservation {
    pub child_id: String,
    pub tracker: AgentTaskFanoutTrackerState,
    pub provider: AgentTaskFanoutProviderState,
    pub worktree: AgentTaskFanoutWorktreeState,
    pub candidate: AgentTaskFanoutCandidateState,
    pub gates: AgentTaskFanoutEvidenceState,
    pub acceptance: AgentTaskFanoutEvidenceState,
    pub pr: AgentTaskFanoutPrState,
    pub findings: Vec<AgentTaskFanoutReviewFinding>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTaskFanoutTrackerState {
    Open,
    Closed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTaskFanoutProviderState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTaskFanoutWorktreeState {
    #[default]
    Clean,
    Dirty,
    Conflicted,
    Missing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTaskFanoutCandidateState {
    pub source_sha: Option<String>,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub current_base_sha: Option<String>,
    pub remote_head_sha: Option<String>,
    /// A durable compare-and-swap receipt proves that a matching remote head
    /// belongs to this supervisor rather than an interrupted mutation window.
    pub publication_receipt_current: bool,
    pub can_rebase: bool,
    pub can_recreate: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTaskFanoutEvidenceState {
    #[default]
    Missing,
    Current,
    Failed,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTaskFanoutPrState {
    #[default]
    Missing,
    OpenChecksPending,
    OpenChecksFailed,
    OpenChecksPassing,
    Merged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskFanoutReviewFinding {
    pub fingerprint: String,
    pub summary: String,
}

/// #10946 owns the graph. Returning `Blocked` prevents mutation for that child
/// while siblings are still reconciled in the same portfolio pass.
pub trait FanoutDependencyResolver {
    fn readiness(&self, child_id: &str) -> FanoutDependencyReadiness;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanoutDependencyReadiness {
    Ready,
    Blocked {
        detail: String,
        evidence_ref: String,
    },
}

pub struct IndependentFanoutDependencies;

impl FanoutDependencyResolver for IndependentFanoutDependencies {
    fn readiness(&self, _child_id: &str) -> FanoutDependencyReadiness {
        FanoutDependencyReadiness::Ready
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioStatus {
    pub schema: &'static str,
    pub fanout_id: String,
    pub revision: u64,
    pub total: usize,
    pub ready: usize,
    pub blocked: usize,
    pub merged: usize,
    pub children: Vec<AgentTaskFanoutPortfolioChildStatus>,
    pub drill_down_ref: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioChildStatus {
    pub child_id: String,
    pub tracker_ref: String,
    pub tracker: String,
    pub source_sha: Option<String>,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub provider: String,
    pub worktree: String,
    pub gates: String,
    pub acceptance: String,
    pub pr: String,
    pub blocker: Option<AgentTaskFanoutPortfolioBlocker>,
    pub next_action: AgentTaskFanoutPortfolioAction,
    pub drill_down_ref: String,
}

impl AgentTaskFanoutPortfolio {
    pub fn new(
        fanout_id: impl Into<String>,
        children: impl IntoIterator<Item = AgentTaskFanoutPortfolioChild>,
    ) -> Self {
        let children = children
            .into_iter()
            .map(|child| (child.child_id.clone(), child))
            .collect();
        Self {
            schema: portfolio_schema(),
            fanout_id: fanout_id.into(),
            children,
            finding_fingerprints: BTreeSet::new(),
            finding_fingerprint_recency: BTreeMap::new(),
            revision: 0,
            updated_at: None,
        }
    }

    /// Reconcile every supplied child. A blocked child never short-circuits a
    /// sibling, and unsafe worktrees are only assigned inspection actions.
    pub fn reconcile(
        &mut self,
        observations: impl IntoIterator<Item = AgentTaskFanoutPortfolioObservation>,
        dependencies: &dyn FanoutDependencyResolver,
    ) -> AgentTaskFanoutPortfolioStatus {
        let observations = observations
            .into_iter()
            .map(|o| (o.child_id.clone(), o))
            .collect::<BTreeMap<_, _>>();
        let generation = self.revision.saturating_add(1);
        let mut portfolio_findings = BTreeSet::new();
        for (child_id, child) in &mut self.children {
            let Some(observation) = observations.get(child_id) else {
                continue;
            };
            // The first observation establishes provenance. Only later movement
            // invalidates evidence that was already attached to the candidate.
            let candidate_changed = child.base_sha != observation.candidate.base_sha
                || child.source_sha != observation.candidate.source_sha
                || child.head_sha != observation.candidate.head_sha;
            let changed_base = child.base_sha.is_some() && candidate_changed;
            if candidate_changed {
                if changed_base {
                    child.evidence_generation = child.evidence_generation.saturating_add(1);
                }
                child.base_sha = observation.candidate.base_sha.clone();
                child.source_sha = observation.candidate.source_sha.clone();
                child.head_sha = observation.candidate.head_sha.clone();
            }
            let findings = observation
                .findings
                .iter()
                .map(|finding| finding.fingerprint.clone())
                .collect::<BTreeSet<_>>();
            // Active findings take retention priority. Historical findings are
            // kept by most-recent observation, with fingerprint ordering
            // breaking ties so replay and restart produce the same snapshot.
            retain_finding_fingerprints(
                &mut child.finding_fingerprints,
                &mut child.finding_fingerprint_recency,
                &findings,
                generation,
                CHILD_FINDING_FINGERPRINT_LIMIT,
            );
            portfolio_findings.extend(findings);
            let (blocker, action) = reconcile_child(
                child,
                observation,
                dependencies.readiness(child_id),
                changed_base,
            );
            child.blocker = blocker;
            child.next_action = Some(action);
        }
        retain_finding_fingerprints(
            &mut self.finding_fingerprints,
            &mut self.finding_fingerprint_recency,
            &portfolio_findings,
            generation,
            PORTFOLIO_FINDING_FINGERPRINT_LIMIT,
        );
        self.revision = generation;
        self.updated_at = Some(Utc::now().to_rfc3339());
        self.status(&observations)
    }

    /// The default projection remains bounded even for much larger portfolios.
    pub fn status(
        &self,
        observations: &BTreeMap<String, AgentTaskFanoutPortfolioObservation>,
    ) -> AgentTaskFanoutPortfolioStatus {
        let (ready, blocked, merged) =
            self.children.iter().fold((0, 0, 0), |totals, (id, child)| {
                let observation = observations.get(id).cloned().unwrap_or_default();
                (
                    totals.0
                        + usize::from(
                            child.blocker.is_none()
                                && observation.acceptance == AgentTaskFanoutEvidenceState::Current
                                && observation.pr == AgentTaskFanoutPrState::OpenChecksPassing
                                && observation.candidate.head_sha
                                    == observation.candidate.remote_head_sha,
                        ),
                    totals.1 + usize::from(child.blocker.is_some()),
                    totals.2 + usize::from(observation.pr == AgentTaskFanoutPrState::Merged),
                )
            });
        let children = self
            .children
            .iter()
            .take(10)
            .map(|(id, child)| {
                let observation = observations.get(id).cloned().unwrap_or_default();
                AgentTaskFanoutPortfolioChildStatus {
                    child_id: id.clone(),
                    tracker_ref: child.tracker_ref.clone(),
                    tracker: format!("{:?}", observation.tracker).to_lowercase(),
                    source_sha: child.source_sha.clone(),
                    base_sha: child.base_sha.clone(),
                    head_sha: child.head_sha.clone(),
                    provider: format!("{:?}", observation.provider).to_lowercase(),
                    worktree: format!("{:?}", observation.worktree).to_lowercase(),
                    gates: format!("{:?}", observation.gates).to_lowercase(),
                    acceptance: format!("{:?}", observation.acceptance).to_lowercase(),
                    pr: format!("{:?}", observation.pr).to_lowercase(),
                    blocker: child.blocker.clone(),
                    next_action: child
                        .next_action
                        .clone()
                        .unwrap_or(AgentTaskFanoutPortfolioAction::None),
                    drill_down_ref: format!("homeboy://fanout/{}/children/{}", self.fanout_id, id),
                }
            })
            .collect();
        AgentTaskFanoutPortfolioStatus {
            schema: AGENT_TASK_FANOUT_PORTFOLIO_STATUS_SCHEMA,
            fanout_id: self.fanout_id.clone(),
            revision: self.revision,
            total: self.children.len(),
            ready,
            blocked,
            merged,
            children,
            drill_down_ref: format!("homeboy://fanout/{}", self.fanout_id),
        }
    }
}

fn retain_finding_fingerprints(
    fingerprints: &mut BTreeSet<String>,
    recency: &mut BTreeMap<String, u64>,
    active: &BTreeSet<String>,
    generation: u64,
    limit: usize,
) {
    fingerprints.extend(active.iter().cloned());
    for fingerprint in active {
        recency.insert(fingerprint.clone(), generation);
    }
    recency.retain(|fingerprint, _| fingerprints.contains(fingerprint));
    let mut retained = fingerprints.iter().cloned().collect::<Vec<_>>();
    retained.sort_by(|left, right| {
        active
            .contains(right)
            .cmp(&active.contains(left))
            .then_with(|| recency.get(right).cmp(&recency.get(left)))
            .then_with(|| left.cmp(right))
    });
    retained.truncate(limit);
    *fingerprints = retained.into_iter().collect();
    recency.retain(|fingerprint, _| fingerprints.contains(fingerprint));
}

/// Runtime boundary for the real tracker, provider, git, gate, review, and PR
/// integrations. The supervisor owns ordering and idempotency; adapters own
/// product-specific APIs and must refresh their observation after a mutation.
pub trait FanoutPortfolioAdapter {
    fn observe(
        &mut self,
        child: &AgentTaskFanoutPortfolioChild,
    ) -> Result<AgentTaskFanoutPortfolioObservation>;
    fn continue_provider(&mut self, _child: &AgentTaskFanoutPortfolioChild) -> Result<()> {
        Ok(())
    }
    fn rebase_candidate(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()>;
    fn recreate_candidate(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()>;
    fn rerun_gates_and_review(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()>;
    fn finalize_or_update_pr(
        &mut self,
        child: &AgentTaskFanoutPortfolioChild,
        force_with_lease: bool,
    ) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioRunReport {
    pub status: AgentTaskFanoutPortfolioStatus,
    pub advanced: Vec<String>,
    pub blocked: Vec<String>,
}

impl AgentTaskFanoutPortfolio {
    /// Executes only the action selected from a fresh observation. Dirty,
    /// conflicted, rejected, and failed children never reach an adapter mutation;
    /// each ready sibling is processed independently.
    pub fn run<A: FanoutPortfolioAdapter>(
        &mut self,
        adapter: &mut A,
        dependencies: &dyn FanoutDependencyResolver,
    ) -> Result<AgentTaskFanoutPortfolioRunReport> {
        let mut observations = Vec::new();
        let mut blocked = Vec::new();
        let children = self.children.values().cloned().collect::<Vec<_>>();
        for child in &children {
            match adapter.observe(child) {
                Ok(observation) => observations.push(observation),
                Err(error) => {
                    self.children
                        .get_mut(&child.child_id)
                        .expect("child exists")
                        .blocker = Some(adapter_blocker(child, "adapter_observe_failed", &error));
                    blocked.push(child.child_id.clone());
                }
            }
        }
        self.reconcile(observations, dependencies);
        let mut advanced = Vec::new();
        let child_ids = self.children.keys().cloned().collect::<Vec<_>>();
        for child_id in child_ids {
            let child = &self.children[&child_id];
            let Some(action) = child.next_action.clone() else {
                continue;
            };
            if child.blocker.is_some() {
                blocked.push(child.child_id.clone());
                continue;
            }
            let result = match action {
                AgentTaskFanoutPortfolioAction::ContinueProvider => {
                    adapter.continue_provider(child)
                }
                AgentTaskFanoutPortfolioAction::RebaseAndRerunGates => {
                    adapter.rebase_candidate(child)
                }
                AgentTaskFanoutPortfolioAction::RecreateCandidateAndRerunGates => {
                    adapter.recreate_candidate(child)
                }
                AgentTaskFanoutPortfolioAction::RerunGates => adapter.rerun_gates_and_review(child),
                AgentTaskFanoutPortfolioAction::ResumeFinalization => {
                    adapter.finalize_or_update_pr(child, false)
                }
                AgentTaskFanoutPortfolioAction::UpdatePrForceWithLease => {
                    adapter.finalize_or_update_pr(child, true)
                }
                AgentTaskFanoutPortfolioAction::AwaitAcceptance
                | AgentTaskFanoutPortfolioAction::InspectBlockedCandidate
                | AgentTaskFanoutPortfolioAction::None => continue,
            };
            if let Err(error) = result {
                let child = &self.children[&child_id];
                self.children
                    .get_mut(&child_id)
                    .expect("child exists")
                    .blocker = Some(adapter_blocker(child, "adapter_action_failed", &error));
                blocked.push(child_id);
                continue;
            }
            advanced.push(child_id);
        }
        // Re-observe after every mutation batch, then persist one convergent
        // snapshot. A process restart repeats only actions whose observation
        // still requires them.
        let mut observations = Vec::new();
        let children = self.children.values().cloned().collect::<Vec<_>>();
        for child in &children {
            match adapter.observe(child) {
                Ok(observation) => observations.push(observation),
                Err(error) => {
                    self.children
                        .get_mut(&child.child_id)
                        .expect("child exists")
                        .blocker = Some(adapter_blocker(child, "adapter_observe_failed", &error));
                    if !blocked.contains(&child.child_id) {
                        blocked.push(child.child_id.clone());
                    }
                }
            }
        }
        let status = self.reconcile(observations, dependencies);
        write_portfolio(self)?;
        Ok(AgentTaskFanoutPortfolioRunReport {
            status,
            advanced,
            blocked,
        })
    }
}

fn adapter_blocker(
    child: &AgentTaskFanoutPortfolioChild,
    code: &str,
    error: &Error,
) -> AgentTaskFanoutPortfolioBlocker {
    AgentTaskFanoutPortfolioBlocker {
        code: code.to_string(),
        detail: error.to_string(),
        evidence_ref: format!(
            "homeboy://fanout/portfolio/children/{}/evidence/{}",
            child.child_id, code
        ),
    }
}

/// Store one convergent portfolio snapshot per fanout. A resume therefore
/// restores finding dedupe and next actions without replaying prior mutations.
pub fn write_portfolio(portfolio: &AgentTaskFanoutPortfolio) -> Result<()> {
    let path = portfolio_path(&portfolio.fanout_id)?;
    let parent = path.parent().expect("portfolio path has parent");
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    let raw = serde_json::to_string_pretty(portfolio).map_err(|error| {
        Error::internal_json(error.to_string(), Some(portfolio.fanout_id.clone()))
    })?;
    fs::write(&path, raw)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

pub fn read_portfolio(fanout_id: &str) -> Result<AgentTaskFanoutPortfolio> {
    let path = portfolio_path(fanout_id)?;
    let raw = fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let portfolio: AgentTaskFanoutPortfolio = serde_json::from_str(&raw).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    validate_portfolio(&portfolio, fanout_id)?;
    Ok(portfolio)
}

pub fn portfolio_exists(fanout_id: &str) -> Result<bool> {
    let path = portfolio_path(fanout_id)?;
    path.try_exists()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn validate_portfolio(portfolio: &AgentTaskFanoutPortfolio, fanout_id: &str) -> Result<()> {
    if portfolio.schema != AGENT_TASK_FANOUT_PORTFOLIO_SCHEMA
        || portfolio.fanout_id != fanout_id
        || portfolio
            .children
            .iter()
            .any(|(id, child)| id != &child.child_id)
    {
        return Err(Error::validation_invalid_argument(
            "fanout_portfolio",
            "persisted fanout portfolio is corrupt or belongs to a different fanout",
            Some(fanout_id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn portfolio_path(fanout_id: &str) -> Result<std::path::PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-fanout-portfolios")
        .join(format!("{}.json", paths::sanitize_path_segment(fanout_id))))
}

fn reconcile_child(
    child: &AgentTaskFanoutPortfolioChild,
    observation: &AgentTaskFanoutPortfolioObservation,
    dependency: FanoutDependencyReadiness,
    changed_base: bool,
) -> (
    Option<AgentTaskFanoutPortfolioBlocker>,
    AgentTaskFanoutPortfolioAction,
) {
    let blocked = |code: &str, detail: String| {
        (
            Some(AgentTaskFanoutPortfolioBlocker {
                code: code.to_string(),
                evidence_ref: format!(
                    "homeboy://fanout/{}/children/{}/evidence/{}",
                    "portfolio", child.child_id, code
                ),
                detail,
            }),
            AgentTaskFanoutPortfolioAction::InspectBlockedCandidate,
        )
    };
    if let FanoutDependencyReadiness::Blocked {
        detail,
        evidence_ref,
    } = dependency
    {
        return (
            Some(AgentTaskFanoutPortfolioBlocker {
                code: "blocked_by_dependency".into(),
                detail,
                evidence_ref,
            }),
            AgentTaskFanoutPortfolioAction::None,
        );
    }
    if observation.tracker == AgentTaskFanoutTrackerState::Unknown {
        return blocked(
            "tracker_unknown",
            "tracker state could not be observed".into(),
        );
    }
    if observation.tracker == AgentTaskFanoutTrackerState::Closed {
        return (None, AgentTaskFanoutPortfolioAction::None);
    }
    if matches!(
        observation.worktree,
        AgentTaskFanoutWorktreeState::Dirty
            | AgentTaskFanoutWorktreeState::Conflicted
            | AgentTaskFanoutWorktreeState::Missing
    ) {
        return blocked(
            "unsafe_worktree",
            format!(
                "{:?} candidate worktree is retained without overwrite",
                observation.worktree
            ),
        );
    }
    if observation.provider == AgentTaskFanoutProviderState::Running {
        return (None, AgentTaskFanoutPortfolioAction::ContinueProvider);
    }
    if observation.provider == AgentTaskFanoutProviderState::Failed {
        return blocked(
            "provider_failed",
            "provider attempt failed; preserve its evidence before retry".into(),
        );
    }
    if observation.gates == AgentTaskFanoutEvidenceState::Unknown {
        return blocked(
            "gate_state_unknown",
            "gate state could not be observed".into(),
        );
    }
    if observation.gates == AgentTaskFanoutEvidenceState::Failed {
        return blocked("gate_failed", "declared deterministic gates failed".into());
    }
    if observation.acceptance == AgentTaskFanoutEvidenceState::Unknown {
        return blocked(
            "acceptance_unknown",
            "acceptance state could not be observed".into(),
        );
    }
    if observation.acceptance == AgentTaskFanoutEvidenceState::Rejected
        || !observation.findings.is_empty()
    {
        return blocked(
            "review_rejected",
            "deduplicated review findings require a new candidate".into(),
        );
    }
    if changed_base || observation.candidate.current_base_sha != observation.candidate.base_sha {
        if !observation.candidate.can_rebase && !observation.candidate.can_recreate {
            return blocked(
                "stale_candidate_unrecoverable",
                "candidate base changed and no safe rebase or recreation path is available".into(),
            );
        }
        return (
            None,
            if observation.candidate.can_rebase {
                AgentTaskFanoutPortfolioAction::RebaseAndRerunGates
            } else if observation.candidate.can_recreate {
                AgentTaskFanoutPortfolioAction::RecreateCandidateAndRerunGates
            } else {
                AgentTaskFanoutPortfolioAction::InspectBlockedCandidate
            },
        );
    }
    if observation.gates != AgentTaskFanoutEvidenceState::Current {
        return (None, AgentTaskFanoutPortfolioAction::RerunGates);
    }
    if observation.acceptance != AgentTaskFanoutEvidenceState::Current {
        return (None, AgentTaskFanoutPortfolioAction::AwaitAcceptance);
    }
    match observation.pr {
        AgentTaskFanoutPrState::Unknown => blocked(
            "pr_state_unknown",
            "PR/check state could not be observed".into(),
        ),
        AgentTaskFanoutPrState::Missing => {
            (None, AgentTaskFanoutPortfolioAction::ResumeFinalization)
        }
        AgentTaskFanoutPrState::OpenChecksFailed => {
            blocked("pr_checks_failed", "PR checks failed".into())
        }
        AgentTaskFanoutPrState::OpenChecksPending => {
            (None, AgentTaskFanoutPortfolioAction::AwaitAcceptance)
        }
        AgentTaskFanoutPrState::OpenChecksPassing
            if observation.candidate.head_sha == observation.candidate.remote_head_sha
                && observation.candidate.publication_receipt_current =>
        {
            (None, AgentTaskFanoutPortfolioAction::None)
        }
        AgentTaskFanoutPrState::OpenChecksPassing => {
            (None, AgentTaskFanoutPortfolioAction::UpdatePrForceWithLease)
        }
        AgentTaskFanoutPrState::Merged => (None, AgentTaskFanoutPortfolioAction::None),
    }
}

fn portfolio_schema() -> String {
    AGENT_TASK_FANOUT_PORTFOLIO_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn child(id: &str) -> AgentTaskFanoutPortfolioChild {
        AgentTaskFanoutPortfolioChild {
            child_id: id.into(),
            tracker_ref: format!("https://tracker/{id}"),
            run_id: format!("run-{id}"),
            source_sha: None,
            base_sha: None,
            head_sha: None,
            evidence_generation: 0,
            finding_fingerprints: BTreeSet::new(),
            finding_fingerprint_recency: BTreeMap::new(),
            blocker: None,
            next_action: None,
        }
    }
    fn observation(id: &str) -> AgentTaskFanoutPortfolioObservation {
        AgentTaskFanoutPortfolioObservation {
            child_id: id.into(),
            tracker: AgentTaskFanoutTrackerState::Open,
            provider: AgentTaskFanoutProviderState::Succeeded,
            worktree: AgentTaskFanoutWorktreeState::Clean,
            candidate: AgentTaskFanoutCandidateState {
                source_sha: Some("source".into()),
                base_sha: Some("base".into()),
                head_sha: Some("head".into()),
                current_base_sha: Some("base".into()),
                remote_head_sha: Some("head".into()),
                publication_receipt_current: true,
                can_rebase: true,
                can_recreate: true,
            },
            gates: AgentTaskFanoutEvidenceState::Current,
            acceptance: AgentTaskFanoutEvidenceState::Current,
            pr: AgentTaskFanoutPrState::Missing,
            findings: vec![],
        }
    }
    #[test]
    fn mixed_portfolio_reconciles_each_child_without_blocking_ready_siblings() {
        let ids = [
            "ready", "stale", "dirty", "conflict", "gate", "review", "merged", "provider",
        ];
        let mut portfolio = AgentTaskFanoutPortfolio::new("mixed", ids.into_iter().map(child));
        let mut states = ids.into_iter().map(observation).collect::<Vec<_>>();
        states[1].candidate.current_base_sha = Some("new-base".into());
        states[2].worktree = AgentTaskFanoutWorktreeState::Dirty;
        states[3].worktree = AgentTaskFanoutWorktreeState::Conflicted;
        states[4].gates = AgentTaskFanoutEvidenceState::Failed;
        states[5].findings.push(AgentTaskFanoutReviewFinding {
            fingerprint: "review-1".into(),
            summary: "fix".into(),
        });
        states[6].pr = AgentTaskFanoutPrState::Merged;
        states[7].provider = AgentTaskFanoutProviderState::Running;
        let status = portfolio.reconcile(states.clone(), &IndependentFanoutDependencies);
        assert_eq!(
            portfolio.children["ready"].next_action,
            Some(AgentTaskFanoutPortfolioAction::ResumeFinalization)
        );
        assert_eq!(
            portfolio.children["stale"].next_action,
            Some(AgentTaskFanoutPortfolioAction::RebaseAndRerunGates)
        );
        assert_eq!(
            portfolio.children["dirty"].blocker.as_ref().unwrap().code,
            "unsafe_worktree"
        );
        assert_eq!(portfolio.children["review"].finding_fingerprints.len(), 1);
        assert_eq!(status.children.len(), 8);
        portfolio.reconcile(states, &IndependentFanoutDependencies);
        assert_eq!(portfolio.children["review"].finding_fingerprints.len(), 1);
    }

    #[test]
    fn finding_fingerprints_are_bounded_and_retain_active_and_recent_evidence() {
        let mut portfolio = AgentTaskFanoutPortfolio::new("bounded-findings", [child("child")]);
        for index in 0..(PORTFOLIO_FINDING_FINGERPRINT_LIMIT + 20) {
            let mut state = observation("child");
            state.findings = vec![AgentTaskFanoutReviewFinding {
                fingerprint: format!("finding-{index:03}"),
                summary: "review evidence".into(),
            }];
            portfolio.reconcile([state], &IndependentFanoutDependencies);
        }

        assert_eq!(
            portfolio.finding_fingerprints.len(),
            PORTFOLIO_FINDING_FINGERPRINT_LIMIT
        );
        assert_eq!(
            portfolio.children["child"].finding_fingerprints.len(),
            CHILD_FINDING_FINGERPRINT_LIMIT
        );
        assert!(portfolio.finding_fingerprints.contains("finding-147"));
        assert!(portfolio.children["child"]
            .finding_fingerprints
            .contains("finding-147"));
        assert!(portfolio.children["child"]
            .finding_fingerprints
            .contains("finding-116"));
        assert!(!portfolio.children["child"]
            .finding_fingerprints
            .contains("finding-115"));

        let mut active = observation("child");
        active.findings = vec![AgentTaskFanoutReviewFinding {
            fingerprint: "finding-000".into(),
            summary: "active again".into(),
        }];
        portfolio.reconcile([active], &IndependentFanoutDependencies);
        assert!(portfolio.finding_fingerprints.contains("finding-000"));
        assert!(
            portfolio.children["child"].finding_fingerprints.len()
                <= CHILD_FINDING_FINGERPRINT_LIMIT
        );
    }

    #[derive(Default)]
    struct FixtureAdapter {
        observations: BTreeMap<String, AgentTaskFanoutPortfolioObservation>,
        actions: Vec<String>,
        fail_observe: BTreeSet<String>,
    }

    impl FanoutPortfolioAdapter for FixtureAdapter {
        fn observe(
            &mut self,
            child: &AgentTaskFanoutPortfolioChild,
        ) -> Result<AgentTaskFanoutPortfolioObservation> {
            if self.fail_observe.contains(&child.child_id) {
                return Err(Error::internal_unexpected("fixture observation failed"));
            }
            Ok(self.observations[&child.child_id].clone())
        }

        fn rebase_candidate(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()> {
            self.actions.push(format!("rebase:{}", child.child_id));
            let observation = self.observations.get_mut(&child.child_id).unwrap();
            observation.candidate.base_sha = observation.candidate.current_base_sha.clone();
            Ok(())
        }

        fn recreate_candidate(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()> {
            self.actions.push(format!("recreate:{}", child.child_id));
            let observation = self.observations.get_mut(&child.child_id).unwrap();
            observation.candidate.base_sha = observation.candidate.current_base_sha.clone();
            Ok(())
        }

        fn rerun_gates_and_review(&mut self, child: &AgentTaskFanoutPortfolioChild) -> Result<()> {
            self.actions.push(format!("gates:{}", child.child_id));
            self.observations.get_mut(&child.child_id).unwrap().gates =
                AgentTaskFanoutEvidenceState::Current;
            Ok(())
        }

        fn finalize_or_update_pr(
            &mut self,
            child: &AgentTaskFanoutPortfolioChild,
            force_with_lease: bool,
        ) -> Result<()> {
            self.actions.push(format!(
                "pr{}:{}",
                if force_with_lease { "-lease" } else { "" },
                child.child_id
            ));
            self.observations.get_mut(&child.child_id).unwrap().pr = AgentTaskFanoutPrState::Merged;
            Ok(())
        }
    }

    #[test]
    fn action_executor_persists_and_converges_after_restart() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut portfolio = AgentTaskFanoutPortfolio::new(
                "restart-fixture",
                [child("ready"), child("stale"), child("dirty")],
            );
            let mut adapter = FixtureAdapter::default();
            adapter
                .observations
                .insert("ready".into(), observation("ready"));
            let mut stale = observation("stale");
            stale.candidate.current_base_sha = Some("new-base".into());
            adapter.observations.insert("stale".into(), stale);
            let mut dirty = observation("dirty");
            dirty.worktree = AgentTaskFanoutWorktreeState::Dirty;
            adapter.observations.insert("dirty".into(), dirty);

            let first = portfolio
                .run(&mut adapter, &IndependentFanoutDependencies)
                .expect("first reconciliation");
            assert_eq!(first.advanced, vec!["ready", "stale"]);
            assert_eq!(first.blocked, vec!["dirty"]);
            assert_eq!(adapter.actions, vec!["pr:ready", "rebase:stale"]);
            assert_eq!(
                read_portfolio("restart-fixture").unwrap().revision,
                portfolio.revision
            );

            let mut restarted = read_portfolio("restart-fixture").expect("durable restart state");
            let second = restarted
                .run(&mut adapter, &IndependentFanoutDependencies)
                .expect("restart reconciliation");
            assert_eq!(second.advanced, vec!["stale"]);
            assert_eq!(second.blocked, vec!["dirty"]);
            assert_eq!(adapter.actions.last(), Some(&"pr:stale".to_string()));
        });
    }

    #[test]
    fn review_ready_requires_a_fresh_pr_head_with_passing_checks() {
        let mut portfolio = AgentTaskFanoutPortfolio::new("review-ready", [child("child")]);
        let mut stale_pr = observation("child");
        stale_pr.pr = AgentTaskFanoutPrState::OpenChecksPassing;
        stale_pr.candidate.remote_head_sha = Some("old-head".into());
        portfolio.reconcile([stale_pr], &IndependentFanoutDependencies);
        assert_eq!(
            portfolio.children["child"].next_action,
            Some(AgentTaskFanoutPortfolioAction::UpdatePrForceWithLease)
        );

        let mut fresh_pr = observation("child");
        fresh_pr.pr = AgentTaskFanoutPrState::OpenChecksPassing;
        fresh_pr.candidate.publication_receipt_current = false;
        portfolio.reconcile([fresh_pr], &IndependentFanoutDependencies);
        assert_eq!(
            portfolio.children["child"].next_action,
            Some(AgentTaskFanoutPortfolioAction::UpdatePrForceWithLease)
        );

        let mut fresh_pr = observation("child");
        fresh_pr.pr = AgentTaskFanoutPrState::OpenChecksPassing;
        portfolio.reconcile([fresh_pr], &IndependentFanoutDependencies);
        assert_eq!(
            portfolio.children["child"].next_action,
            Some(AgentTaskFanoutPortfolioAction::None)
        );
    }

    #[test]
    fn corrupt_portfolio_is_rejected_instead_of_recreated() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path = portfolio_path("corrupt").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                r#"{"schema":"wrong","fanout_id":"corrupt","children":{}}"#,
            )
            .unwrap();
            assert!(read_portfolio("corrupt").is_err());
        });
    }

    #[test]
    fn production_adapter_fixture_recreates_once_without_a_duplicate_continuation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut portfolio = AgentTaskFanoutPortfolio::new("recreate", [child("child")]);
            let mut state = observation("child");
            state.candidate.current_base_sha = Some("advanced-base".into());
            state.candidate.can_rebase = false;
            let mut adapter = FixtureAdapter {
                observations: [("child".to_string(), state)].into_iter().collect(),
                actions: Vec::new(),
                fail_observe: BTreeSet::new(),
            };

            portfolio
                .run(&mut adapter, &IndependentFanoutDependencies)
                .unwrap();

            assert_eq!(adapter.actions, vec!["recreate:child"]);
        });
    }

    #[test]
    fn adapter_errors_are_durable_child_blockers_and_do_not_stop_siblings() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut portfolio =
                AgentTaskFanoutPortfolio::new("adapter-errors", [child("bad"), child("good")]);
            let mut adapter = FixtureAdapter {
                observations: [
                    ("bad".to_string(), observation("bad")),
                    ("good".to_string(), observation("good")),
                ]
                .into_iter()
                .collect(),
                actions: Vec::new(),
                fail_observe: BTreeSet::new(),
            };
            adapter.observations.get_mut("bad").unwrap().worktree =
                AgentTaskFanoutWorktreeState::Dirty;
            portfolio
                .run(&mut adapter, &IndependentFanoutDependencies)
                .unwrap();
            assert_eq!(adapter.actions, vec!["pr:good"]);

            // An unavailable observation is a durable, child-scoped failure;
            // it must not erase the completed sibling's reconciliation.
            adapter.fail_observe.insert("bad".to_string());
            let report = portfolio
                .run(&mut adapter, &IndependentFanoutDependencies)
                .unwrap();
            assert!(report.blocked.contains(&"bad".to_string()));
            assert_eq!(
                read_portfolio("adapter-errors").unwrap().children["bad"]
                    .blocker
                    .as_ref()
                    .unwrap()
                    .code,
                "adapter_observe_failed"
            );
        });
    }
}
