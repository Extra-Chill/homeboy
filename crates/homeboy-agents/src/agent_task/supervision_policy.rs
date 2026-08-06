//! Declarative resource policy for a long-running provider session.
//!
//! The sibling `command_policy` module bounds *what* an agent may run. This
//! module bounds *how much it may consume while running it*. They are
//! complements, and
//! neither substitutes for the other: a command policy cannot see an agent that
//! stays inside its allowed commands and still grows to nine gigabytes across
//! forty child processes, and an execution budget counting attempts cannot see
//! it either. Homeboy waited on the process boundary and learned the cost only
//! after the run ended (#7015).
//!
//! The model is deliberately small and runtime-neutral:
//!
//! - A **metric** is a number any host can observe about any provider —
//!   wall-clock time, resident memory, child processes, time since the
//!   workspace last changed. Nothing here knows what a provider is.
//! - A **budget** is one threshold on one metric plus the action to take.
//! - The **ladder** is `warn` → `nudge` → `stop`. Several budgets on the same
//!   metric are how an operator expresses "tell me at 6 GiB, stop it at 10".
//!
//! ## Unobserved is not zero
//!
//! Every metric on a sample is optional and an absent metric never breaches a
//! budget. A non-Unix host has no `ps` and therefore no memory reading; a cook
//! with no destination worktree has no progress reading. Reporting those as
//! `0` would make every budget fire immediately on exactly the hosts that can
//! least afford a spurious kill, so absence is carried as absence all the way
//! to the comparison.
//!
//! ## What this does not do
//!
//! The ladder's `nudge` rung records a decision and surfaces it; it does not
//! yet inject structured feedback into a live provider session, because
//! Homeboy has no channel into a running provider's reasoning (#7451, #7530).
//! Until that channel exists `nudge` is an operator-visible escalation
//! *between* `warn` and `stop`, which is honest, rather than a promise the
//! runtime cannot keep.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const AGENT_SUPERVISION_POLICY_SCHEMA: &str = "homeboy/agent-supervision-policy/v1";

/// Fallback explanation when neither the budget nor the policy supplies one.
pub const DEFAULT_SUPERVISION_REASON: &str =
    "this budget is declared by the operator's Homeboy supervision policy for this host";

/// Guidance attached to a `stop`, so the run's evidence explains the kill
/// rather than leaving a reader to infer it from a dead process.
pub const SUPERVISION_STOP_REMEDIATION: &str =
    "The provider process tree was terminated because it exceeded a declared resource budget. \
Narrow the task, raise the budget in agent_task.supervision_policy, or route the expensive \
step to CI.";

/// Guidance attached to a `warn` or `nudge`.
pub const SUPERVISION_WARNING_REMEDIATION: &str =
    "The run continues. Watch it, or raise the budget in agent_task.supervision_policy if this \
is the expected cost of the task.";

pub(crate) fn agent_supervision_policy_schema() -> String {
    AGENT_SUPERVISION_POLICY_SCHEMA.to_string()
}

/// A quantity any host can observe about any provider session.
///
/// Kept to numbers that a process table and a worktree answer, because the
/// moment a metric needs provider-specific knowledge it stops being a core
/// concern and belongs in an extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSupervisionMetric {
    /// Wall-clock seconds since provider execution began.
    ElapsedSeconds,
    /// Resident memory held by the provider's process tree, in MiB.
    RssMib,
    /// Processes observed under the controller while the provider runs.
    ChildProcesses,
    /// Seconds since the destination worktree last changed.
    ///
    /// This is the stall detector, and it is the metric that distinguishes an
    /// expensive run from a wedged one. A cook burning memory while writing
    /// files is working; a cook burning memory having written nothing for
    /// twenty minutes is the failure mode this whole module exists for.
    NoProgressSeconds,
}

impl AgentSupervisionMetric {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ElapsedSeconds => "elapsed_seconds",
            Self::RssMib => "rss_mib",
            Self::ChildProcesses => "child_processes",
            Self::NoProgressSeconds => "no_progress_seconds",
        }
    }

    /// Unit suffix for operator-facing rendering.
    pub fn unit(&self) -> &'static str {
        match self {
            Self::ElapsedSeconds | Self::NoProgressSeconds => "s",
            Self::RssMib => " MiB",
            Self::ChildProcesses => " process(es)",
        }
    }
}

/// The supervision ladder, ordered from least to most intervention.
///
/// Declaration order *is* the severity order — `Ord` is derived and the
/// escalation logic compares actions directly — so a new rung must be inserted
/// at its severity position, not appended.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentSupervisionAction {
    /// Record and surface the breach. The run continues untouched.
    #[default]
    Warn,
    /// Record and surface the breach as an escalation. The run continues.
    /// Reserved for structured feedback into the session once a channel into a
    /// running provider exists (#7451, #7530).
    Nudge,
    /// Terminate the provider's process tree.
    Stop,
}

impl AgentSupervisionAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Nudge => "nudge",
            Self::Stop => "stop",
        }
    }

    /// True when reaching this rung ends the provider session.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

/// One threshold on one metric.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSupervisionBudget {
    pub metric: AgentSupervisionMetric,
    /// The breach is strict: `observed > limit`. An operator writing
    /// `child_processes: 8` means eight is fine.
    pub limit: u64,
    #[serde(default)]
    pub action: AgentSupervisionAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AgentSupervisionBudget {
    pub fn new(metric: AgentSupervisionMetric, limit: u64, action: AgentSupervisionAction) -> Self {
        Self {
            metric,
            limit,
            action,
            reason: None,
        }
    }

    pub fn with_reason(
        metric: AgentSupervisionMetric,
        limit: u64,
        action: AgentSupervisionAction,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            metric,
            limit,
            action,
            reason: Some(reason.into()),
        }
    }

    pub fn is_breached(&self, observed: u64) -> bool {
        observed > self.limit
    }
}

/// The resource budgets a provider session runs under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSupervisionPolicy {
    #[serde(default = "agent_supervision_policy_schema")]
    pub schema: String,
    /// Budgets applied to any backend without its own entry in `backends`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budgets: Vec<AgentSupervisionBudget>,
    /// Per-backend budget sets, keyed by executor backend id.
    ///
    /// A backend entry **replaces** `budgets` rather than extending it. Unlike
    /// a command policy — where a forgotten deny rule re-opens a hazard and
    /// extension is the safe default — a resource budget is a single number per
    /// metric, and merging two of them would leave an operator unable to state
    /// "this backend legitimately needs more" without also fighting the global
    /// value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub backends: BTreeMap<String, Vec<AgentSupervisionBudget>>,
    /// Policy-wide explanation used when a budget carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Default for AgentSupervisionPolicy {
    fn default() -> Self {
        Self {
            schema: agent_supervision_policy_schema(),
            budgets: Vec::new(),
            backends: BTreeMap::new(),
            reason: None,
        }
    }
}

impl AgentSupervisionPolicy {
    /// True when the policy constrains nothing anywhere. An unconstrained
    /// policy is the default, so supervision is opt-in: a host that has not
    /// declared budgets behaves exactly as it did before this existed.
    pub fn is_unconstrained(&self) -> bool {
        self.budgets.is_empty() && self.backends.values().all(Vec::is_empty)
    }

    /// The budgets that apply to `backend`.
    pub fn effective_budgets(&self, backend: Option<&str>) -> &[AgentSupervisionBudget] {
        backend
            .and_then(|backend| self.backends.get(backend))
            .map(Vec::as_slice)
            .unwrap_or(&self.budgets)
    }

    /// True when no budget applies to this backend, so [`Self::evaluate`]
    /// cannot produce a decision for it however extreme the sample is.
    pub fn is_unconstrained_for(&self, backend: Option<&str>) -> bool {
        self.effective_budgets(backend).is_empty()
    }

    /// Decide what the sample warrants.
    ///
    /// At most one decision per metric: the most severe breached budget, and
    /// among equally severe ones the highest limit, because "you passed 10 GiB"
    /// is a strictly more informative statement than "you passed 6 GiB" when
    /// both are true and both merely warn.
    pub fn evaluate(
        &self,
        backend: Option<&str>,
        sample: &AgentSupervisionSample,
    ) -> Vec<AgentSupervisionDecision> {
        let mut by_metric: BTreeMap<AgentSupervisionMetric, AgentSupervisionDecision> =
            BTreeMap::new();
        for budget in self.effective_budgets(backend) {
            // An unobserved metric never breaches. This is the load-bearing
            // line for non-Unix hosts and for cooks with no worktree.
            let Some(observed) = sample.observed(budget.metric) else {
                continue;
            };
            if !budget.is_breached(observed) {
                continue;
            }
            let candidate = self.decision(budget, observed);
            let supersedes = match by_metric.get(&budget.metric) {
                Some(existing) => {
                    (candidate.action, candidate.limit) > (existing.action, existing.limit)
                }
                None => true,
            };
            if supersedes {
                by_metric.insert(budget.metric, candidate);
            }
        }
        by_metric.into_values().collect()
    }

    fn decision(&self, budget: &AgentSupervisionBudget, observed: u64) -> AgentSupervisionDecision {
        let reason = budget
            .reason
            .clone()
            .or_else(|| self.reason.clone())
            .unwrap_or_else(|| DEFAULT_SUPERVISION_REASON.to_string());
        let remediation = if budget.action.is_terminal() {
            SUPERVISION_STOP_REMEDIATION
        } else {
            SUPERVISION_WARNING_REMEDIATION
        };
        AgentSupervisionDecision {
            metric: budget.metric,
            action: budget.action,
            limit: budget.limit,
            observed,
            reason,
            remediation: remediation.to_string(),
        }
    }
}

/// One observation of a running provider session.
///
/// Every field is optional and `None` means "not observed", never "zero".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSupervisionSample {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_processes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_progress_seconds: Option<u64>,
}

impl AgentSupervisionSample {
    pub fn observed(&self, metric: AgentSupervisionMetric) -> Option<u64> {
        match metric {
            AgentSupervisionMetric::ElapsedSeconds => self.elapsed_seconds,
            AgentSupervisionMetric::RssMib => self.rss_mib,
            AgentSupervisionMetric::ChildProcesses => self.child_processes,
            AgentSupervisionMetric::NoProgressSeconds => self.no_progress_seconds,
        }
    }

    /// True when nothing at all was observed, so recording the sample would add
    /// a row of nulls to the timeline instead of evidence.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A supervision decision, as recorded in run evidence.
///
/// Carries the observation alongside the threshold so a post-hoc reader can see
/// *why* a run was stopped without reconstructing the policy that was in force
/// at the time — the policy is config and config changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSupervisionDecision {
    pub metric: AgentSupervisionMetric,
    pub action: AgentSupervisionAction,
    pub limit: u64,
    pub observed: u64,
    pub reason: String,
    pub remediation: String,
}

impl AgentSupervisionDecision {
    /// Bounded operator-facing line: what was seen, against what, and what
    /// Homeboy did about it.
    pub fn message(&self) -> String {
        format!(
            "{}: {}{} exceeds the {} budget of {}{} ({})",
            self.action.as_str(),
            self.observed,
            self.metric.unit(),
            self.metric.as_str(),
            self.limit,
            self.metric.unit(),
            self.reason,
        )
    }
}

/// The most severe action among a set of decisions.
pub fn highest_supervision_action(
    decisions: &[AgentSupervisionDecision],
) -> Option<AgentSupervisionAction> {
    decisions.iter().map(|decision| decision.action).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(budgets: Vec<AgentSupervisionBudget>) -> AgentSupervisionPolicy {
        AgentSupervisionPolicy {
            budgets,
            ..AgentSupervisionPolicy::default()
        }
    }

    fn sample() -> AgentSupervisionSample {
        AgentSupervisionSample {
            elapsed_seconds: Some(600),
            rss_mib: Some(9_216),
            child_processes: Some(41),
            no_progress_seconds: Some(120),
        }
    }

    #[test]
    fn an_undeclared_policy_supervises_nothing() {
        let policy = AgentSupervisionPolicy::default();

        assert!(policy.is_unconstrained());
        assert!(policy.is_unconstrained_for(Some("opencode")));
        assert!(policy.evaluate(None, &sample()).is_empty());
    }

    #[test]
    fn a_breached_budget_reports_the_observation_and_the_threshold() {
        // The dogfood shape from #7015: high peak RSS across many child
        // processes, which was only visible after the run.
        let policy = policy(vec![AgentSupervisionBudget::with_reason(
            AgentSupervisionMetric::RssMib,
            8_192,
            AgentSupervisionAction::Warn,
            "this box has 15Gi and four agents on it",
        )]);

        let decisions = policy.evaluate(None, &sample());

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].metric, AgentSupervisionMetric::RssMib);
        assert_eq!(decisions[0].action, AgentSupervisionAction::Warn);
        assert_eq!(decisions[0].observed, 9_216);
        assert_eq!(decisions[0].limit, 8_192);
        assert!(decisions[0].message().contains("9216 MiB"));
        assert!(decisions[0].message().contains("four agents"));
        assert_eq!(decisions[0].remediation, SUPERVISION_WARNING_REMEDIATION);
    }

    #[test]
    fn the_limit_is_a_ceiling_not_a_trigger() {
        let policy = policy(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::ChildProcesses,
            41,
            AgentSupervisionAction::Stop,
        )]);

        assert!(policy.evaluate(None, &sample()).is_empty());
    }

    #[test]
    fn the_ladder_reports_only_the_highest_rung_reached_on_a_metric() {
        // "Tell me at 6 GiB, stop it at 8" is two budgets on one metric, and a
        // sample past both is one decision — the stop — not two.
        let policy = policy(vec![
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                6_144,
                AgentSupervisionAction::Warn,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                7_168,
                AgentSupervisionAction::Nudge,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                8_192,
                AgentSupervisionAction::Stop,
            ),
        ]);

        let decisions = policy.evaluate(None, &sample());

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].action, AgentSupervisionAction::Stop);
        assert_eq!(decisions[0].limit, 8_192);
        assert_eq!(decisions[0].remediation, SUPERVISION_STOP_REMEDIATION);
        assert_eq!(
            highest_supervision_action(&decisions),
            Some(AgentSupervisionAction::Stop)
        );
    }

    #[test]
    fn equally_severe_budgets_report_the_higher_threshold_crossed() {
        let policy = policy(vec![
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                2_048,
                AgentSupervisionAction::Warn,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                8_192,
                AgentSupervisionAction::Warn,
            ),
        ]);

        let decisions = policy.evaluate(None, &sample());

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].limit, 8_192);
    }

    #[test]
    fn every_breached_metric_gets_its_own_decision() {
        let policy = policy(vec![
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                8_192,
                AgentSupervisionAction::Warn,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::ChildProcesses,
                16,
                AgentSupervisionAction::Nudge,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::NoProgressSeconds,
                60,
                AgentSupervisionAction::Stop,
            ),
        ]);

        let decisions = policy.evaluate(None, &sample());

        assert_eq!(decisions.len(), 3);
        assert_eq!(
            highest_supervision_action(&decisions),
            Some(AgentSupervisionAction::Stop)
        );
    }

    #[test]
    fn an_unobserved_metric_never_breaches() {
        // A non-Unix host has no process table and a worktree-less cook has no
        // progress reading. Neither may be killed for it.
        let policy = policy(vec![
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                1,
                AgentSupervisionAction::Stop,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::ChildProcesses,
                1,
                AgentSupervisionAction::Stop,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::NoProgressSeconds,
                1,
                AgentSupervisionAction::Stop,
            ),
        ]);
        let unobserved = AgentSupervisionSample {
            elapsed_seconds: Some(600),
            ..AgentSupervisionSample::default()
        };

        assert!(policy.evaluate(None, &unobserved).is_empty());
        assert!(AgentSupervisionSample::default().is_empty());
        assert!(!unobserved.is_empty());
    }

    #[test]
    fn a_backend_entry_replaces_the_global_budgets_rather_than_merging_them() {
        // A backend that legitimately needs more memory must be able to say so
        // without the global number still firing underneath it.
        let mut backends = BTreeMap::new();
        backends.insert(
            "opencode".to_string(),
            vec![AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                12_288,
                AgentSupervisionAction::Stop,
            )],
        );
        let policy = AgentSupervisionPolicy {
            budgets: vec![AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                4_096,
                AgentSupervisionAction::Stop,
            )],
            backends,
            ..AgentSupervisionPolicy::default()
        };

        assert!(policy.evaluate(Some("opencode"), &sample()).is_empty());
        assert_eq!(policy.evaluate(Some("claude"), &sample()).len(), 1);
        assert_eq!(policy.evaluate(None, &sample()).len(), 1);
        assert!(!policy.is_unconstrained_for(Some("opencode")));
    }

    #[test]
    fn a_backend_with_an_empty_budget_set_opts_out_of_supervision() {
        let mut backends = BTreeMap::new();
        backends.insert("bench".to_string(), Vec::new());
        let policy = AgentSupervisionPolicy {
            budgets: vec![AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                1,
                AgentSupervisionAction::Stop,
            )],
            backends,
            ..AgentSupervisionPolicy::default()
        };

        assert!(policy.is_unconstrained_for(Some("bench")));
        assert!(policy.evaluate(Some("bench"), &sample()).is_empty());
        assert!(!policy.is_unconstrained());
    }

    #[test]
    fn budget_reason_beats_policy_reason_which_beats_the_default() {
        let reasoned = AgentSupervisionPolicy {
            budgets: vec![
                AgentSupervisionBudget::with_reason(
                    AgentSupervisionMetric::RssMib,
                    1,
                    AgentSupervisionAction::Warn,
                    "memory is the binding constraint here",
                ),
                AgentSupervisionBudget::new(
                    AgentSupervisionMetric::ChildProcesses,
                    1,
                    AgentSupervisionAction::Warn,
                ),
            ],
            reason: Some("shared 15Gi VPS".to_string()),
            ..AgentSupervisionPolicy::default()
        };

        let decisions = reasoned.evaluate(None, &sample());

        let rss = decisions
            .iter()
            .find(|decision| decision.metric == AgentSupervisionMetric::RssMib)
            .expect("rss decision");
        assert_eq!(rss.reason, "memory is the binding constraint here");
        let children = decisions
            .iter()
            .find(|decision| decision.metric == AgentSupervisionMetric::ChildProcesses)
            .expect("child process decision");
        assert_eq!(children.reason, "shared 15Gi VPS");

        let bare = policy(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::RssMib,
            1,
            AgentSupervisionAction::Warn,
        )]);
        assert_eq!(
            bare.evaluate(None, &sample())[0].reason,
            DEFAULT_SUPERVISION_REASON
        );
    }

    #[test]
    fn the_ladder_orders_from_least_to_most_intervention() {
        assert!(AgentSupervisionAction::Warn < AgentSupervisionAction::Nudge);
        assert!(AgentSupervisionAction::Nudge < AgentSupervisionAction::Stop);
        assert!(!AgentSupervisionAction::Nudge.is_terminal());
        assert!(AgentSupervisionAction::Stop.is_terminal());
        assert_eq!(
            AgentSupervisionAction::default(),
            AgentSupervisionAction::Warn
        );
    }

    #[test]
    fn policy_round_trips_through_json_with_schema_and_action_defaults() {
        let raw = serde_json::json!({
            "budgets": [
                { "metric": "rss_mib", "limit": 8192, "action": "stop", "reason": "15Gi box" },
                { "metric": "no_progress_seconds", "limit": 900 }
            ],
            "backends": {
                "opencode": [{ "metric": "child_processes", "limit": 32, "action": "nudge" }]
            },
            "reason": "shared host"
        });

        let policy: AgentSupervisionPolicy = serde_json::from_value(raw).expect("policy");

        assert_eq!(policy.schema, AGENT_SUPERVISION_POLICY_SCHEMA);
        assert_eq!(policy.budgets.len(), 2);
        // An omitted action is the gentlest rung, never the most severe one.
        assert_eq!(policy.budgets[1].action, AgentSupervisionAction::Warn);
        assert_eq!(
            policy.backends["opencode"][0].metric,
            AgentSupervisionMetric::ChildProcesses
        );

        let encoded = serde_json::to_value(&policy).expect("encode");
        let decoded: AgentSupervisionPolicy = serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, policy);
    }

    #[test]
    fn decisions_round_trip_through_their_durable_projection() {
        let decision = policy(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::NoProgressSeconds,
            60,
            AgentSupervisionAction::Stop,
        )])
        .evaluate(None, &sample())
        .remove(0);

        let value = serde_json::to_value(&decision).expect("encode");
        assert_eq!(value["metric"], "no_progress_seconds");
        assert_eq!(value["action"], "stop");
        let restored: AgentSupervisionDecision = serde_json::from_value(value).expect("decode");
        assert_eq!(restored, decision);
    }
}
