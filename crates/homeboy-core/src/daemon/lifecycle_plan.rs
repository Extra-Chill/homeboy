//! The daemon lifecycle plan: the wire vocabulary of the one recovery planner.
//!
//! There is one daemon state directory and one job store, so there is one
//! planner: [`super::recovery_actions::plan_recovery`] reads one authoritative
//! [`DaemonStatus`] and decides both the executable repair steps and the single
//! next lifecycle action. This module names that action as a [`Plan`], so
//! remote callers (`homeboy daemon plan`, schema
//! `homeboy/daemon-lifecycle-plan/v1`) and the mutating entry points read the
//! same verdict `daemon recover` follows.
//!
//! The outcomes:
//!
//! 1. [`Plan::Blocked`]: something could not be read, a live daemon process
//!    owns no lease, or the evidence authorizes no recovery.
//! 2. [`Plan::Wait`]: live work stands in the way of the next step.
//! 3. [`Plan::NeedsAttestation`]: a dead lease's jobs have no proof that their
//!    workload is gone. Only an operator can supply it.
//! 4. [`Plan::Transition`]: the repair the status report authorizes.
//! 5. [`Plan::Converged`]: nothing to do.

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::DaemonStatus;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "plan")]
pub enum Plan {
    Converged,
    Transition { step: Step },
    NeedsAttestation(Attestation),
    Wait { cause: WaitCause, reason: String },
    Blocked { cause: BlockCause, reason: String },
}

/// What the planner is waiting for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitCause {
    /// A dead lease's job still has a live workload process.
    LiveWorkload,
    /// A stale daemon still owns jobs; it is replaced when they finish.
    BusyStaleDaemon,
}

/// Why the planner refuses to act.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockCause {
    /// The lease or status could not be read. Another repair (corrupt-lease
    /// handling) may still apply.
    Unreadable,
    /// A live daemon process holds no lease; anything could race it.
    UnleasedProcess,
    /// The status evidence authorizes no automatic recovery.
    Unauthorized,
}

/// One recovery transition over the single daemon store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum Step {
    /// Run the repair the freshness report authorizes. `codes` are the
    /// [`super::recovery_actions`] step codes, in execution order.
    Repair {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease_id: Option<String>,
        codes: Vec<String>,
    },
}

/// The one operator confirmation the status report cannot prove itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attestation {
    pub lease_id: String,
    pub state_dir: std::path::PathBuf,
    /// The dead lease's complete active job set (compare-and-swap scope).
    pub job_ids: Vec<Uuid>,
    pub confirmation: String,
}

/// The confirmation flag an attestation requires.
pub const CONFIRM_WORKLOAD_PROCESSES_ABSENT: &str =
    super::recovery_actions::CONFIRM_WORKLOAD_PROCESSES_ABSENT;

/// The lifecycle plan for one authoritative status report.
pub fn plan(status: &DaemonStatus) -> Plan {
    super::recovery_actions::plan_recovery(status).lifecycle
}

/// The lifecycle plan for this process's daemon store. An unreadable status is
/// a [`BlockCause::Unreadable`] verdict, never an error.
pub fn plan_local() -> Plan {
    match super::read_status() {
        Ok(status) => plan(&status),
        Err(error) => Plan::Blocked {
            cause: BlockCause::Unreadable,
            reason: format!("daemon status is unreadable: {}", error.message),
        },
    }
}

/// The shared safety gate for every mutating daemon entry point.
///
/// Returns why `planned` forbids a lease or job-store mutation right now: a
/// dead lease still has a live workload process, or a live daemon process
/// holds no lease and could own the store being changed. Everything else is
/// left to the entry point's own, narrower preconditions.
pub fn mutation_refusal(planned: &Plan) -> Option<String> {
    match planned {
        Plan::Wait {
            cause: WaitCause::LiveWorkload,
            reason,
        } => Some(format!("live workload: {reason}")),
        Plan::Blocked {
            cause: BlockCause::UnleasedProcess,
            reason,
        } => Some(format!("unleased daemon process: {reason}")),
        _ => None,
    }
}

/// Refuse `entry_point` when the current lifecycle plan forbids mutation.
pub fn ensure_mutation_allowed(entry_point: &str) -> Result<()> {
    ensure_mutation_allowed_with(entry_point, plan_local)
}

fn ensure_mutation_allowed_with(entry_point: &str, replan: impl FnOnce() -> Plan) -> Result<()> {
    let Some(reason) = mutation_refusal(&replan()) else {
        return Ok(());
    };
    let mut error = Error::validation_invalid_argument(
        "daemon_lifecycle",
        format!("{entry_point} refused by the daemon lifecycle plan: {reason}"),
        None,
        Some(vec![
            "Run `homeboy daemon recover --dry-run` for the one next action.".to_string(),
        ]),
    );
    error.details["classification"] = json!("lifecycle_refused");
    Err(error)
}

#[cfg(test)]
#[path = "lifecycle_plan_tests.rs"]
mod tests;
