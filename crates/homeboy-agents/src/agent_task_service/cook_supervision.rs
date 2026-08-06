//! Supervision of a running Cook against its declared resource budgets.
//!
//! The sibling `cook_activity` module answers "what is the provider doing, and
//! what is it holding". This module turns that observation into a decision.
//! Without it Homeboy sampled a running cook every fifteen seconds, wrote the
//! sample down, and then waited for the process boundary regardless of what the
//! sample said — the orchestrator watched the fire and did not own an
//! extinguisher (#7015).
//!
//! Three things live here and nothing else:
//!
//! 1. **Projection.** A [`CookProviderActivity`] sample becomes a generic
//!    [`AgentSupervisionSample`]. This is the seam that keeps the policy
//!    runtime-neutral: the policy never sees a worktree, a `ps` row, or a
//!    provider.
//! 2. **Stall accounting.** "Seconds since the worktree last changed" is the
//!    one metric that cannot be read from a single sample, so the supervisor
//!    carries the minimum state needed to derive it across ticks.
//! 3. **Escalation.** A breached budget is announced once, and again only when
//!    it climbs the ladder. A heartbeat that repeats the same warning every
//!    fifteen seconds trains an operator to ignore the whole channel.
//!
//! Enforcement of a `stop` is not here: this module decides, and the cook
//! orchestration acts, because terminating a process tree is a lifecycle
//! concern and this file deliberately owns no lifecycle.

use std::collections::BTreeMap;

use crate::agent_task::{
    AgentSupervisionAction, AgentSupervisionDecision, AgentSupervisionMetric,
    AgentSupervisionPolicy, AgentSupervisionSample,
};
use homeboy_core::{defaults, Error, Result};

use super::cook_activity::CookProviderActivity;

/// Load the host's supervision policy.
///
/// A malformed document is an error rather than a silently ignored one. The
/// alternative is an operator who declared a stop budget, believes the host is
/// protected, and is not — which is worse than having declared nothing, because
/// it is indistinguishable from working.
pub fn resolve_supervision_policy() -> Result<AgentSupervisionPolicy> {
    match defaults::load_config().agent_task.supervision_policy {
        Some(raw) => serde_json::from_value::<AgentSupervisionPolicy>(raw).map_err(|error| {
            Error::validation_invalid_argument(
                "agent_task.supervision_policy",
                format!(
                    "configured agent_task.supervision_policy is not a valid \
homeboy/agent-supervision-policy/v1 document: {error}"
                ),
                None,
                Some(vec![
                    "Example: homeboy config set /agent_task/supervision_policy '{\"budgets\":[{\"metric\":\"rss_mib\",\"limit\":8192,\"action\":\"stop\",\"reason\":\"shared 15Gi host\"}]}' --json".to_string(),
                ]),
            )
        }),
        None => Ok(AgentSupervisionPolicy::default()),
    }
}

/// What one supervision tick concluded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CookSupervisionTick {
    /// The generic sample, for the durable resource timeline.
    pub sample: AgentSupervisionSample,
    /// Decisions reached *on this tick*: a first breach, or an escalation of
    /// one already reported. A budget that stays breached at the same rung
    /// yields nothing, so the supervision channel stays worth reading.
    pub decisions: Vec<AgentSupervisionDecision>,
    /// True on the single tick that first reaches `stop`. The caller terminates
    /// the provider tree exactly once; a repeated stop would signal a tree that
    /// is already being torn down.
    pub stop_now: bool,
}

impl CookSupervisionTick {
    /// One bounded line for the heartbeat's detail field.
    pub fn detail_line(&self) -> Option<String> {
        if self.decisions.is_empty() {
            return None;
        }
        Some(
            self.decisions
                .iter()
                .map(AgentSupervisionDecision::message)
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// True when the tick produced nothing worth writing down.
    pub fn is_empty(&self) -> bool {
        self.sample.is_empty() && self.decisions.is_empty()
    }
}

/// Stateful supervisor for one provider execution.
pub struct CookSupervisor {
    policy: AgentSupervisionPolicy,
    backend: Option<String>,
    /// Worktree progress as last observed: `(files_changed, commits_written)`.
    ///
    /// A commit clears the pending-edit count, so progress has to be the pair.
    /// Watching `files_changed` alone would read a provider that just committed
    /// as a provider that has undone its work.
    last_progress: Option<(usize, usize)>,
    /// Elapsed reading at which `last_progress` was first seen.
    last_progress_at_seconds: u64,
    /// Highest rung already announced per metric.
    announced: BTreeMap<AgentSupervisionMetric, AgentSupervisionAction>,
    stop_issued: bool,
}

impl CookSupervisor {
    pub fn new(policy: AgentSupervisionPolicy, backend: Option<String>) -> Self {
        Self {
            policy,
            backend,
            last_progress: None,
            last_progress_at_seconds: 0,
            announced: BTreeMap::new(),
            stop_issued: false,
        }
    }

    /// True when no budget applies to this execution, so no decision can be
    /// reached however extreme the samples get.
    ///
    /// Sampling continues regardless: supervision is opt-in but *observation*
    /// is not, and the resource timeline is what turns "was this run
    /// expensive?" from a question you had to be watching to answer into one
    /// the run record answers afterwards.
    pub fn is_inactive(&self) -> bool {
        self.policy.is_unconstrained_for(self.backend.as_deref())
    }

    /// True once a `stop` has been issued for this execution.
    pub fn stop_issued(&self) -> bool {
        self.stop_issued
    }

    /// Fold one activity sample into the supervision state.
    pub fn observe(&mut self, activity: &CookProviderActivity) -> CookSupervisionTick {
        let sample = self.project(activity);
        if self.is_inactive() {
            return CookSupervisionTick {
                sample,
                decisions: Vec::new(),
                stop_now: false,
            };
        }
        let evaluated = self.policy.evaluate(self.backend.as_deref(), &sample);
        let mut decisions = Vec::new();
        for decision in evaluated {
            let escalates = match self.announced.get(&decision.metric) {
                Some(announced) => decision.action > *announced,
                None => true,
            };
            if escalates {
                self.announced.insert(decision.metric, decision.action);
                decisions.push(decision);
            }
        }
        let stop_now = !self.stop_issued
            && decisions
                .iter()
                .any(|decision| decision.action.is_terminal());
        if stop_now {
            self.stop_issued = true;
        }
        CookSupervisionTick {
            sample,
            decisions,
            stop_now,
        }
    }

    /// Project a Cook-shaped observation onto the runtime-neutral metrics.
    ///
    /// Every absent field stays absent. The temptation to substitute a zero is
    /// exactly the bug that would make a Windows host — where there is no `ps`
    /// and therefore no memory or process reading at all — breach a memory
    /// budget on its first heartbeat.
    fn project(&mut self, activity: &CookProviderActivity) -> AgentSupervisionSample {
        AgentSupervisionSample {
            elapsed_seconds: activity.elapsed_seconds,
            rss_mib: activity.tree_rss_mib.or(activity.rss_mib),
            child_processes: activity.child_processes.map(|count| count as u64),
            no_progress_seconds: self.no_progress_seconds(activity),
        }
    }

    /// Seconds since the worktree last changed, or `None` when progress is not
    /// measurable at all.
    ///
    /// Unmeasurable is the honest answer for a cook with no destination
    /// worktree and for one whose worktree could not be read: a stall budget
    /// must not fire on a cook whose progress nobody can see.
    fn no_progress_seconds(&mut self, activity: &CookProviderActivity) -> Option<u64> {
        let elapsed = activity.elapsed_seconds?;
        let progress = (
            activity.files_changed?,
            activity.commits_written.unwrap_or(0),
        );
        if self.last_progress != Some(progress) {
            self.last_progress = Some(progress);
            self.last_progress_at_seconds = elapsed;
        }
        Some(elapsed.saturating_sub(self.last_progress_at_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::AgentSupervisionBudget;
    use std::collections::BTreeMap;

    fn supervisor(budgets: Vec<AgentSupervisionBudget>) -> CookSupervisor {
        CookSupervisor::new(
            AgentSupervisionPolicy {
                budgets,
                ..AgentSupervisionPolicy::default()
            },
            Some("opencode".to_string()),
        )
    }

    fn activity(elapsed: u64, rss_mib: u64, children: usize) -> CookProviderActivity {
        CookProviderActivity {
            elapsed_seconds: Some(elapsed),
            tree_rss_mib: Some(rss_mib),
            child_processes: Some(children),
            files_changed: Some(0),
            commits_written: Some(0),
            ..CookProviderActivity::default()
        }
    }

    #[test]
    fn an_undeclared_policy_samples_without_deciding_anything() {
        // Supervision is opt-in: an unconfigured host keeps behaving exactly as
        // it did, while the evidence gets richer for free.
        let mut supervisor = supervisor(Vec::new());

        let tick = supervisor.observe(&activity(60, 9_216, 41));

        assert!(supervisor.is_inactive());
        assert!(tick.decisions.is_empty());
        assert!(!tick.stop_now);
        assert_eq!(tick.sample.rss_mib, Some(9_216));
        assert_eq!(tick.sample.child_processes, Some(41));
        assert_eq!(tick.detail_line(), None);
    }

    #[test]
    fn a_breach_is_announced_once_and_again_only_when_it_escalates() {
        // A heartbeat that repeats the same warning every fifteen seconds
        // teaches an operator to stop reading heartbeats.
        let mut supervisor = supervisor(vec![
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                4_096,
                AgentSupervisionAction::Warn,
            ),
            AgentSupervisionBudget::new(
                AgentSupervisionMetric::RssMib,
                8_192,
                AgentSupervisionAction::Stop,
            ),
        ]);

        let first = supervisor.observe(&activity(15, 5_000, 4));
        assert_eq!(first.decisions.len(), 1);
        assert_eq!(first.decisions[0].action, AgentSupervisionAction::Warn);
        assert!(!first.stop_now);

        // Still warning, still over: nothing new to say.
        let repeat = supervisor.observe(&activity(30, 6_000, 4));
        assert!(repeat.decisions.is_empty());
        assert!(!repeat.stop_now);

        let escalated = supervisor.observe(&activity(45, 9_216, 4));
        assert_eq!(escalated.decisions.len(), 1);
        assert_eq!(escalated.decisions[0].action, AgentSupervisionAction::Stop);
        assert!(escalated.stop_now);
        assert!(supervisor.stop_issued());

        // The stop is issued exactly once; the tree is already being torn down.
        let after = supervisor.observe(&activity(60, 9_216, 4));
        assert!(!after.stop_now);
    }

    #[test]
    fn a_stall_is_measured_from_the_last_worktree_change() {
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::NoProgressSeconds,
            60,
            AgentSupervisionAction::Stop,
        )]);

        // Baseline: the first observation is the reference, not a stall.
        let first = supervisor.observe(&activity(15, 100, 2));
        assert_eq!(first.sample.no_progress_seconds, Some(0));
        assert!(first.decisions.is_empty());

        let stalling = supervisor.observe(&activity(70, 100, 2));
        assert_eq!(stalling.sample.no_progress_seconds, Some(55));
        assert!(stalling.decisions.is_empty());

        let stalled = supervisor.observe(&activity(90, 100, 2));
        assert_eq!(stalled.sample.no_progress_seconds, Some(75));
        assert!(stalled.stop_now);
    }

    #[test]
    fn writing_a_file_resets_the_stall_clock() {
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::NoProgressSeconds,
            60,
            AgentSupervisionAction::Stop,
        )]);

        supervisor.observe(&activity(15, 100, 2));
        assert_eq!(
            supervisor
                .observe(&activity(60, 100, 2))
                .sample
                .no_progress_seconds,
            Some(45)
        );

        let mut edited = activity(70, 100, 2);
        edited.files_changed = Some(3);
        assert_eq!(
            supervisor.observe(&edited).sample.no_progress_seconds,
            Some(0)
        );

        let mut still_editing = activity(100, 100, 2);
        still_editing.files_changed = Some(3);
        assert_eq!(
            supervisor
                .observe(&still_editing)
                .sample
                .no_progress_seconds,
            Some(30)
        );
    }

    #[test]
    fn a_commit_is_progress_even_though_it_clears_the_pending_edit_count() {
        // A provider that commits leaves a clean tree. Watching `files_changed`
        // alone would read that as having undone its work and start a stall
        // clock on a cook that just succeeded at something.
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::NoProgressSeconds,
            60,
            AgentSupervisionAction::Stop,
        )]);

        let mut editing = activity(15, 100, 2);
        editing.files_changed = Some(4);
        supervisor.observe(&editing);

        let mut committed = activity(80, 100, 2);
        committed.files_changed = Some(0);
        committed.commits_written = Some(1);

        let tick = supervisor.observe(&committed);

        assert_eq!(tick.sample.no_progress_seconds, Some(0));
        assert!(tick.decisions.is_empty());
        assert!(!tick.stop_now);
    }

    #[test]
    fn an_unmeasurable_worktree_never_starts_a_stall_clock() {
        // A cook with no destination worktree, or one whose worktree could not
        // be read, has no progress signal. Killing it for that would punish the
        // probe's failure rather than the provider's.
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::NoProgressSeconds,
            1,
            AgentSupervisionAction::Stop,
        )]);
        let unmeasurable = CookProviderActivity {
            elapsed_seconds: Some(6_000),
            ..CookProviderActivity::default()
        };

        let tick = supervisor.observe(&unmeasurable);

        assert_eq!(tick.sample.no_progress_seconds, None);
        assert!(tick.decisions.is_empty());
        assert!(!tick.stop_now);
    }

    #[test]
    fn a_host_that_cannot_read_its_process_table_is_never_stopped_for_memory() {
        // The non-Unix case, and the failed-`ps` case. Both must decline to
        // decide rather than decide on a fabricated zero.
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::new(
            AgentSupervisionMetric::RssMib,
            0,
            AgentSupervisionAction::Stop,
        )]);
        let unsampled = CookProviderActivity {
            elapsed_seconds: Some(600),
            files_changed: Some(2),
            ..CookProviderActivity::default()
        };

        let tick = supervisor.observe(&unsampled);

        assert_eq!(tick.sample.rss_mib, None);
        assert_eq!(tick.sample.child_processes, None);
        assert!(tick.decisions.is_empty());
        assert!(!tick.stop_now);
    }

    #[test]
    fn a_backend_specific_budget_supervises_only_that_backend() {
        let mut backends = BTreeMap::new();
        backends.insert(
            "opencode".to_string(),
            vec![AgentSupervisionBudget::new(
                AgentSupervisionMetric::ChildProcesses,
                8,
                AgentSupervisionAction::Stop,
            )],
        );
        let policy = AgentSupervisionPolicy {
            backends,
            ..AgentSupervisionPolicy::default()
        };

        let mut supervised = CookSupervisor::new(policy.clone(), Some("opencode".to_string()));
        assert!(supervised.observe(&activity(15, 100, 41)).stop_now);

        let mut unsupervised = CookSupervisor::new(policy, Some("claude".to_string()));
        assert!(unsupervised.is_inactive());
        assert!(!unsupervised.observe(&activity(15, 100, 41)).stop_now);
    }

    #[test]
    fn the_detail_line_states_the_observation_the_threshold_and_the_reason() {
        let mut supervisor = supervisor(vec![AgentSupervisionBudget::with_reason(
            AgentSupervisionMetric::RssMib,
            8_192,
            AgentSupervisionAction::Stop,
            "this box has 15Gi and four agents on it",
        )]);

        let tick = supervisor.observe(&activity(15, 9_216, 41));
        let line = tick.detail_line().expect("a decision renders");

        assert!(line.starts_with("stop:"));
        assert!(line.contains("9216 MiB"));
        assert!(line.contains("8192 MiB"));
        assert!(line.contains("four agents"));
        assert!(!tick.is_empty());
    }

    #[test]
    fn a_malformed_policy_document_does_not_parse_into_a_permissive_policy() {
        // `resolve_supervision_policy` turns this into a hard error rather than
        // silently supervising nothing: an operator who declared a stop budget
        // and got a silently ignored one believes the host is protected and is
        // not.
        let parsed = serde_json::from_value::<AgentSupervisionPolicy>(serde_json::json!({
            "budgets": [{ "metric": "gigabytes", "limit": 8 }]
        }));

        assert!(parsed.is_err());
    }
}
