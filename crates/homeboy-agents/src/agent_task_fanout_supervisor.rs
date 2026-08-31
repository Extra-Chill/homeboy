//! Durable, dependency-agnostic portfolio reconciliation for fanout children.
//!
//! Dependency readiness deliberately enters through [`FanoutDependencyResolver`].
//! The graph and stacked-base semantics belong to #10946; this supervisor only
//! consumes its answer while reconciling otherwise independent children.

use chrono::Utc;
use fs4::fs_std::FileExt;
use homeboy_core::{paths, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};

pub const AGENT_TASK_FANOUT_PORTFOLIO_SCHEMA: &str = "homeboy/agent-task-fanout-portfolio/v1";
pub const AGENT_TASK_FANOUT_PORTFOLIO_STATUS_SCHEMA: &str =
    "homeboy/agent-task-fanout-portfolio-status/v1";
/// Maximum inactive historical findings retained per portfolio. Every finding
/// in the current observation is retained regardless of this limit.
pub const PORTFOLIO_INACTIVE_FINDING_FINGERPRINT_LIMIT: usize = 128;
/// Maximum inactive historical findings retained per child. Every finding in
/// the current child observation is retained regardless of this limit.
pub const CHILD_INACTIVE_FINDING_FINGERPRINT_LIMIT: usize = 32;
/// Child rows projected by the default [`AgentTaskFanoutPortfolio::status`]
/// page. Deliberately unchanged from the previous hard-coded `take(10)`: the
/// fix makes truncation *visible*, it does not widen the default page.
pub const PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT: usize = 10;

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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFanoutTrackerState {
    Open,
    Closed,
    /// The recipe declared the tracker identity, but no forge observation is
    /// available for its current state.
    DeclaredUnobserved,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFanoutProviderState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFanoutEvidenceState {
    #[default]
    Missing,
    Current,
    Failed,
    Rejected,
    Unknown,
}

/// Serialized snake_case, not via `Debug`. `OpenChecksPassing` is the wire
/// string `open_checks_passing`; before #11821 the `Debug`-lowercasing
/// projection shipped `opencheckspassing`, which was neither snake_case nor
/// stable across a Rust-side variant rename.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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
    /// Total children in the portfolio, before any page limit was applied.
    /// `ready`/`blocked`/`merged` are likewise whole-portfolio counts, never
    /// page-scoped.
    pub total: usize,
    pub ready: usize,
    pub blocked: usize,
    pub merged: usize,
    /// Number of child rows in `children` on this page. Mirrors
    /// [`AgentTaskDiscoveryReport::count`] so a consumer never has to infer a
    /// page size from the array length.
    pub count: usize,
    /// The applied page limit, echoed back so consumers can tell a capped
    /// projection from a complete one. `None` when every child was projected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// `true` when the page limit left children unprojected. Previously the
    /// projection silently `take(10)`-ed, so a 25-child wave reported 10 rows
    /// with no way for a consumer to know rows were missing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Offset to pass as the next page's cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    pub children: Vec<AgentTaskFanoutPortfolioChildStatus>,
    pub drill_down_ref: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskFanoutPortfolioChildStatus {
    pub child_id: String,
    pub tracker_ref: String,
    pub tracker: AgentTaskFanoutTrackerState,
    pub source_sha: Option<String>,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub provider: AgentTaskFanoutProviderState,
    pub worktree: AgentTaskFanoutWorktreeState,
    pub gates: AgentTaskFanoutEvidenceState,
    pub acceptance: AgentTaskFanoutEvidenceState,
    pub pr: AgentTaskFanoutPrState,
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
            // Current evidence is complete. Bound only inactive history by
            // most-recent observation, with fingerprint ordering breaking ties
            // so replay and restart produce the same durable snapshot.
            retain_finding_fingerprints(
                &mut child.finding_fingerprints,
                &mut child.finding_fingerprint_recency,
                &findings,
                generation,
                CHILD_INACTIVE_FINDING_FINGERPRINT_LIMIT,
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
            PORTFOLIO_INACTIVE_FINDING_FINGERPRINT_LIMIT,
        );
        self.revision = generation;
        self.updated_at = Some(Utc::now().to_rfc3339());
        self.status(&observations)
    }

    /// The default projection remains bounded even for much larger portfolios,
    /// and reports the bound so truncation is visible rather than silent.
    pub fn status(
        &self,
        observations: &BTreeMap<String, AgentTaskFanoutPortfolioObservation>,
    ) -> AgentTaskFanoutPortfolioStatus {
        self.status_page(observations, Some(PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT), 0)
    }

    /// Paged projection. `limit: None` projects every child; `cursor` is a
    /// zero-based offset into the portfolio's child order. Aggregate counts
    /// (`total`, `ready`, `blocked`, `merged`) always describe the whole
    /// portfolio, not the returned page.
    pub fn status_page(
        &self,
        observations: &BTreeMap<String, AgentTaskFanoutPortfolioObservation>,
        limit: Option<usize>,
        cursor: usize,
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
        let total = self.children.len();
        let cursor = cursor.min(total);
        let end = limit
            .map(|limit| cursor.saturating_add(limit).min(total))
            .unwrap_or(total);
        let truncated = end < total;
        let children: Vec<_> = self
            .children
            .iter()
            .skip(cursor)
            .take(end - cursor)
            .map(|(id, child)| {
                let observation = observations.get(id).cloned().unwrap_or_default();
                AgentTaskFanoutPortfolioChildStatus {
                    child_id: id.clone(),
                    tracker_ref: child.tracker_ref.clone(),
                    tracker: observation.tracker,
                    source_sha: child.source_sha.clone(),
                    base_sha: child.base_sha.clone(),
                    head_sha: child.head_sha.clone(),
                    provider: observation.provider,
                    worktree: observation.worktree,
                    gates: observation.gates,
                    acceptance: observation.acceptance,
                    pr: observation.pr,
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
            total,
            ready,
            blocked,
            merged,
            count: children.len(),
            limit,
            truncated,
            next_cursor: truncated.then_some(end),
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
    inactive_limit: usize,
) {
    fingerprints.extend(active.iter().cloned());
    for fingerprint in active {
        recency.insert(fingerprint.clone(), generation);
    }
    recency.retain(|fingerprint, _| fingerprints.contains(fingerprint));
    let mut inactive = fingerprints
        .iter()
        .filter(|fingerprint| !active.contains(*fingerprint))
        .cloned()
        .collect::<Vec<_>>();
    inactive.sort_by(|left, right| {
        recency
            .get(right)
            .cmp(&recency.get(left))
            .then_with(|| left.cmp(right))
    });
    inactive.truncate(inactive_limit);
    *fingerprints = active.iter().cloned().chain(inactive).collect();
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
    homeboy_core::engine::local_files::create_dir_all_durably(parent)?;
    let raw = serde_json::to_string_pretty(portfolio).map_err(|error| {
        Error::internal_json(error.to_string(), Some(portfolio.fanout_id.clone()))
    })?;
    homeboy_core::io::write_output_file_atomically(
        &path,
        raw,
        homeboy_core::io::OutputWriteOptions::file(),
    )
    .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

/// Serialize every mutating resume for one fanout. This lock is deliberately
/// outside the batch receipt lock: resume may take that narrower lock while
/// persisting exact effects, but no receipt path acquires this lock in reverse.
pub fn with_portfolio_resume_lock<T>(
    fanout_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let path = portfolio_path(fanout_id)?.with_extension("resume.lock");
    let parent = path.parent().expect("portfolio resume lock has parent");
    homeboy_core::engine::local_files::create_dir_all_durably(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    lock.lock_exclusive()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let result = operation();
    let _ = FileExt::unlock(&lock);
    result
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
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;
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
    fn declared_unobserved_tracker_preserves_identity_without_a_missing_tracker_blocker() {
        let mut portfolio = AgentTaskFanoutPortfolio::new("declared-tracker", [child("child")]);
        let mut state = observation("child");
        state.tracker = AgentTaskFanoutTrackerState::DeclaredUnobserved;

        let status = portfolio.reconcile([state], &IndependentFanoutDependencies);

        assert_eq!(status.children[0].tracker_ref, "https://tracker/child");
        assert_eq!(
            status.children[0].tracker,
            AgentTaskFanoutTrackerState::DeclaredUnobserved
        );
        assert!(status.children[0].blocker.is_none());
        assert_eq!(
            status.children[0].next_action,
            AgentTaskFanoutPortfolioAction::ResumeFinalization
        );
    }

    #[test]
    fn finding_fingerprints_retain_all_active_evidence_and_bound_recent_history() {
        let mut portfolio = AgentTaskFanoutPortfolio::new("bounded-findings", [child("child")]);
        for index in 0..150 {
            let mut state = observation("child");
            state.findings = vec![AgentTaskFanoutReviewFinding {
                fingerprint: format!("history-{index:03}"),
                summary: "review evidence".into(),
            }];
            portfolio.reconcile([state], &IndependentFanoutDependencies);
        }

        let mut active = observation("child");
        active.findings = (0..150)
            .map(|index| AgentTaskFanoutReviewFinding {
                fingerprint: format!("active-{index:03}"),
                summary: "active review evidence".into(),
            })
            .collect();
        portfolio.reconcile([active.clone()], &IndependentFanoutDependencies);

        assert_eq!(
            portfolio.finding_fingerprints.len(),
            150 + PORTFOLIO_INACTIVE_FINDING_FINGERPRINT_LIMIT
        );
        assert_eq!(
            portfolio.children["child"].finding_fingerprints.len(),
            150 + CHILD_INACTIVE_FINDING_FINGERPRINT_LIMIT
        );
        assert!(portfolio.finding_fingerprints.contains("active-000"));
        assert!(portfolio.finding_fingerprints.contains("active-149"));
        assert!(portfolio.children["child"]
            .finding_fingerprints
            .contains("active-000"));
        assert!(portfolio.children["child"]
            .finding_fingerprints
            .contains("active-149"));
        assert!(!portfolio.finding_fingerprints.contains("history-021"));
        assert!(portfolio.finding_fingerprints.contains("history-022"));
        assert!(!portfolio.children["child"]
            .finding_fingerprints
            .contains("history-117"));
        assert!(portfolio.children["child"]
            .finding_fingerprints
            .contains("history-118"));

        let expected_portfolio = portfolio.finding_fingerprints.clone();
        let expected_child = portfolio.children["child"].finding_fingerprints.clone();
        portfolio.reconcile([active], &IndependentFanoutDependencies);
        assert_eq!(portfolio.finding_fingerprints, expected_portfolio);
        assert_eq!(
            portfolio.children["child"].finding_fingerprints,
            expected_child
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
    fn concurrent_resumes_serialize_mutation_persistence_and_cleanup() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_portfolio(&AgentTaskFanoutPortfolio::new(
                "concurrent-resumes",
                [child("child")],
            ))
            .unwrap();
            let observation = Arc::new(Mutex::new(observation("child")));
            let mutations = Arc::new(Mutex::new(0_u8));
            let cleanups = Arc::new(Mutex::new(0_u8));
            let cleanup_receipt = Arc::new(Mutex::new(false));
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release_rx = Arc::new(Mutex::new(release_rx));

            struct SharedAdapter {
                observation: Arc<Mutex<AgentTaskFanoutPortfolioObservation>>,
                mutations: Arc<Mutex<u8>>,
            }
            impl FanoutPortfolioAdapter for SharedAdapter {
                fn observe(
                    &mut self,
                    _child: &AgentTaskFanoutPortfolioChild,
                ) -> Result<AgentTaskFanoutPortfolioObservation> {
                    Ok(self.observation.lock().unwrap().clone())
                }
                fn rebase_candidate(
                    &mut self,
                    _child: &AgentTaskFanoutPortfolioChild,
                ) -> Result<()> {
                    unreachable!()
                }
                fn recreate_candidate(
                    &mut self,
                    _child: &AgentTaskFanoutPortfolioChild,
                ) -> Result<()> {
                    unreachable!()
                }
                fn rerun_gates_and_review(
                    &mut self,
                    _child: &AgentTaskFanoutPortfolioChild,
                ) -> Result<()> {
                    unreachable!()
                }
                fn finalize_or_update_pr(
                    &mut self,
                    _child: &AgentTaskFanoutPortfolioChild,
                    _force_with_lease: bool,
                ) -> Result<()> {
                    *self.mutations.lock().unwrap() += 1;
                    self.observation.lock().unwrap().pr = AgentTaskFanoutPrState::Merged;
                    Ok(())
                }
            }

            let spawn_resume = |index: u8, wait_for_release: bool| {
                let observation = Arc::clone(&observation);
                let mutations = Arc::clone(&mutations);
                let cleanups = Arc::clone(&cleanups);
                let cleanup_receipt = Arc::clone(&cleanup_receipt);
                let entered_tx = entered_tx.clone();
                let release_rx = Arc::clone(&release_rx);
                std::thread::spawn(move || {
                    with_portfolio_resume_lock("concurrent-resumes", || {
                        entered_tx.send(index).unwrap();
                        if wait_for_release {
                            release_rx.lock().unwrap().recv().unwrap();
                        }
                        let mut portfolio = read_portfolio("concurrent-resumes")?;
                        portfolio.run(
                            &mut SharedAdapter {
                                observation,
                                mutations,
                            },
                            &IndependentFanoutDependencies,
                        )?;
                        let mut receipt = cleanup_receipt.lock().unwrap();
                        if !*receipt {
                            *cleanups.lock().unwrap() += 1;
                            *receipt = true;
                        }
                        Ok(())
                    })
                })
            };
            let first = spawn_resume(0, true);
            assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
            let second = spawn_resume(1, false);
            assert!(entered_rx.recv_timeout(Duration::from_millis(100)).is_err());
            release_tx.send(()).unwrap();
            assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
            for thread in [first, second] {
                thread.join().unwrap().unwrap();
            }
            assert_eq!(*mutations.lock().unwrap(), 1);
            assert_eq!(*cleanups.lock().unwrap(), 1);
            assert_eq!(read_portfolio("concurrent-resumes").unwrap().revision, 4);
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

    /// The status projection is a wire contract, not a `Debug` dump. Every
    /// observation enum must serialize as snake_case so a Rust-side variant
    /// rename cannot silently change the emitted string.
    #[test]
    fn observation_states_serialize_as_snake_case_strings() {
        fn wire<T: Serialize>(value: T) -> String {
            serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .into()
        }

        assert_eq!(wire(AgentTaskFanoutTrackerState::Open), "open");
        assert_eq!(wire(AgentTaskFanoutTrackerState::Closed), "closed");
        assert_eq!(
            wire(AgentTaskFanoutTrackerState::DeclaredUnobserved),
            "declared_unobserved"
        );
        assert_eq!(wire(AgentTaskFanoutTrackerState::Unknown), "unknown");

        assert_eq!(wire(AgentTaskFanoutProviderState::Pending), "pending");
        assert_eq!(wire(AgentTaskFanoutProviderState::Running), "running");
        assert_eq!(wire(AgentTaskFanoutProviderState::Succeeded), "succeeded");
        assert_eq!(wire(AgentTaskFanoutProviderState::Failed), "failed");

        assert_eq!(wire(AgentTaskFanoutWorktreeState::Clean), "clean");
        assert_eq!(wire(AgentTaskFanoutWorktreeState::Dirty), "dirty");
        assert_eq!(wire(AgentTaskFanoutWorktreeState::Conflicted), "conflicted");
        assert_eq!(wire(AgentTaskFanoutWorktreeState::Missing), "missing");

        assert_eq!(wire(AgentTaskFanoutEvidenceState::Missing), "missing");
        assert_eq!(wire(AgentTaskFanoutEvidenceState::Current), "current");
        assert_eq!(wire(AgentTaskFanoutEvidenceState::Failed), "failed");
        assert_eq!(wire(AgentTaskFanoutEvidenceState::Rejected), "rejected");
        assert_eq!(wire(AgentTaskFanoutEvidenceState::Unknown), "unknown");

        assert_eq!(wire(AgentTaskFanoutPrState::Missing), "missing");
        assert_eq!(
            wire(AgentTaskFanoutPrState::OpenChecksPending),
            "open_checks_pending"
        );
        assert_eq!(
            wire(AgentTaskFanoutPrState::OpenChecksFailed),
            "open_checks_failed"
        );
        // The regression this contract exists for: the former
        // `format!("{:?}", ..).to_lowercase()` projection emitted
        // `opencheckspassing`.
        assert_eq!(
            wire(AgentTaskFanoutPrState::OpenChecksPassing),
            "open_checks_passing"
        );
        assert_eq!(wire(AgentTaskFanoutPrState::Merged), "merged");
        assert_eq!(wire(AgentTaskFanoutPrState::Unknown), "unknown");
    }

    /// The multi-word PR states are the only variants whose wire string the
    /// typed projection changed, so they are pinned round-trip in both
    /// directions rather than serialize-only.
    #[test]
    fn pr_state_round_trips_through_its_snake_case_wire_string() {
        for state in [
            AgentTaskFanoutPrState::Missing,
            AgentTaskFanoutPrState::OpenChecksPending,
            AgentTaskFanoutPrState::OpenChecksFailed,
            AgentTaskFanoutPrState::OpenChecksPassing,
            AgentTaskFanoutPrState::Merged,
            AgentTaskFanoutPrState::Unknown,
        ] {
            let encoded = serde_json::to_string(&state).unwrap();
            let decoded: AgentTaskFanoutPrState = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, state);
        }
        assert_eq!(
            serde_json::from_str::<AgentTaskFanoutPrState>("\"open_checks_passing\"").unwrap(),
            AgentTaskFanoutPrState::OpenChecksPassing
        );
    }

    /// The status projection carries typed states, so a serialized child row
    /// exposes them as snake_case strings rather than `Debug` output.
    #[test]
    fn child_status_rows_serialize_observation_states_as_snake_case() {
        let mut portfolio = AgentTaskFanoutPortfolio::new("typed-status", [child("child")]);
        let mut state = observation("child");
        state.pr = AgentTaskFanoutPrState::OpenChecksPassing;
        let status = portfolio.reconcile([state], &IndependentFanoutDependencies);

        assert_eq!(
            status.children[0].pr,
            AgentTaskFanoutPrState::OpenChecksPassing
        );
        let encoded = serde_json::to_value(&status).unwrap();
        let row = &encoded["children"][0];
        assert_eq!(row["pr"], "open_checks_passing");
        assert_eq!(row["tracker"], "open");
        assert_eq!(row["provider"], "succeeded");
        assert_eq!(row["worktree"], "clean");
        assert_eq!(row["gates"], "current");
        assert_eq!(row["acceptance"], "current");
    }

    /// A wave larger than the default page used to report ten rows with no
    /// signal that any were withheld.
    #[test]
    fn oversized_portfolio_reports_visible_truncation_with_whole_portfolio_totals() {
        let ids = (0..25)
            .map(|index| format!("child-{index:02}"))
            .collect::<Vec<_>>();
        let mut portfolio =
            AgentTaskFanoutPortfolio::new("oversized", ids.iter().map(|id| child(id)));
        let states = ids.iter().map(|id| observation(id)).collect::<Vec<_>>();

        let status = portfolio.reconcile(states, &IndependentFanoutDependencies);

        // The default page size is unchanged; only its visibility is new.
        assert_eq!(status.children.len(), PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT);
        assert_eq!(status.count, PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT);
        assert_eq!(status.total, 25);
        assert_eq!(status.limit, Some(PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT));
        assert!(status.truncated);
        assert_eq!(status.next_cursor, Some(10));

        let encoded = serde_json::to_value(&status).unwrap();
        assert_eq!(encoded["truncated"], true);
        assert_eq!(encoded["total"], 25);
        assert_eq!(encoded["count"], 10);
        assert_eq!(encoded["next_cursor"], 10);
    }

    /// A portfolio that fits the page is not truncated, and the truncation
    /// fields stay out of the serialized payload entirely.
    #[test]
    fn portfolio_within_the_page_limit_is_not_marked_truncated() {
        let ids = ["a", "b", "c"];
        let mut portfolio = AgentTaskFanoutPortfolio::new("small", ids.into_iter().map(child));
        let status = portfolio.reconcile(
            ids.into_iter().map(observation).collect::<Vec<_>>(),
            &IndependentFanoutDependencies,
        );

        assert_eq!(status.count, 3);
        assert_eq!(status.total, 3);
        assert!(!status.truncated);
        assert_eq!(status.next_cursor, None);

        let encoded = serde_json::to_value(&status).unwrap();
        assert!(encoded.get("truncated").is_none());
        assert!(encoded.get("next_cursor").is_none());
    }

    /// `next_cursor` is a real offset: following it yields the next distinct
    /// rows, and an unlimited page projects every child.
    #[test]
    fn status_pages_walk_the_portfolio_without_repeating_or_dropping_children() {
        let ids = (0..25)
            .map(|index| format!("child-{index:02}"))
            .collect::<Vec<_>>();
        let mut portfolio = AgentTaskFanoutPortfolio::new("paged", ids.iter().map(|id| child(id)));
        let states = ids.iter().map(|id| observation(id)).collect::<Vec<_>>();
        portfolio.reconcile(states.clone(), &IndependentFanoutDependencies);
        let observations = states
            .into_iter()
            .map(|state| (state.child_id.clone(), state))
            .collect::<BTreeMap<_, _>>();

        let mut walked = Vec::new();
        let mut cursor = Some(0);
        while let Some(offset) = cursor {
            let page = portfolio.status_page(
                &observations,
                Some(PORTFOLIO_STATUS_DEFAULT_PAGE_LIMIT),
                offset,
            );
            assert_eq!(page.total, 25);
            walked.extend(page.children.iter().map(|row| row.child_id.clone()));
            cursor = page.next_cursor;
        }
        assert_eq!(walked, ids);

        let everything = portfolio.status_page(&observations, None, 0);
        assert_eq!(everything.count, 25);
        assert_eq!(everything.limit, None);
        assert!(!everything.truncated);
        assert_eq!(everything.next_cursor, None);
    }
}
