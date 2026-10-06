//! Daemon lifecycle planner (#15557, slice C2).
//!
//! [`plan`] maps a [`DaemonView`] to exactly one next action. It is a pure
//! function of the view: no I/O, no clock, no process probes. Every recovery
//! entry point will eventually ask this one function what to do instead of
//! re-deriving liveness itself (slices C3/C4). Nothing calls it yet.
//!
//! The outcomes, checked in this order so the safest answer always wins:
//!
//! 1. [`Plan::Blocked`]: something could not be read, or a live daemon
//!    process owns no lease. The planner refuses to guess.
//! 2. [`Plan::Wait`]: live work stands in the way of the next step.
//! 3. [`Plan::NeedsAttestation`]: a dead generation's jobs have no proof that
//!    their workload is gone. Only an operator can supply it.
//! 4. [`Plan::Transition`]: one step whose precondition the view proves.
//! 5. [`Plan::Converged`]: nothing to do.
//!
//! Invariants (tested exhaustively in `lifecycle_plan_tests.rs`):
//! - A transition never targets a generation that still has live work.
//! - `NeedsAttestation` appears only when an unproven orphan exists, and it
//!   names that generation's complete active job set (the store refuses any
//!   other set).
//! - A live generation is never stopped by PID when it is externally
//!   supervised.

use serde::Serialize;
use uuid::Uuid;

use super::lifecycle::{
    BinaryFreshness, DaemonView, GenerationLifecycle, GenerationView, JobCustody,
    RegistryObservation, Supervision,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "plan")]
pub enum Plan {
    Converged,
    Transition { step: Step },
    NeedsAttestation(Attestation),
    Wait { reason: String },
    Blocked { cause: BlockCause, reason: String },
}

/// Why the planner refuses to act.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockCause {
    /// The registry or a lease could not be read. Another repair (legacy
    /// lease migration, corrupt-lease handling) may still apply.
    Unreadable,
    /// A live daemon process holds no lease; anything could race it.
    UnleasedProcess,
    /// A stale daemon has no lease credential to prove a stop is safe.
    UnknownSupervision,
}

/// One recovery step. Each names the exact generation it applies to, so the
/// executor (C3) can re-check the same precondition at apply time.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum Step {
    /// Terminalize jobs of a dead generation whose workload is proven gone
    /// (dead child, reused PID, or recorded terminal evidence).
    ReconcileProvenJobs {
        lease_id: String,
        job_ids: Vec<Uuid>,
    },
    /// Start a replacement that resumes checkpointed in-daemon driver work
    /// (#15426). No attestation: the checkpoint is the proof.
    ResumeDriverWork {
        lease_id: String,
        job_ids: Vec<Uuid>,
    },
    /// Move admission from a dead owner to the live generation (#15456).
    ClaimAdmission {
        from_lease_id: String,
        to_lease_id: String,
    },
    /// Nothing live serves, and the admission owner is dead: start a daemon.
    StartDaemon,
    /// Stop an idle Homeboy-supervised daemon whose binary is stale or
    /// replaced. Its startup credential authorizes a lease-bound stop.
    StopByLease { lease_id: String },
    /// Stop an idle externally supervised daemon through its own lifecycle
    /// endpoint. Never by PID (#15437).
    StopViaLifecycleEndpoint { lease_id: String },
    /// Remove a dead or stopped generation with no jobs, or an idle draining
    /// one, from the registry.
    RetireGeneration { lease_id: String },
}

/// The one operator confirmation the view cannot prove itself.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Attestation {
    pub lease_id: String,
    pub state_dir: std::path::PathBuf,
    /// The generation's complete active job set (compare-and-swap scope).
    pub job_ids: Vec<Uuid>,
    pub confirmation: &'static str,
}

/// The confirmation flag an attestation requires.
pub const CONFIRM_WORKLOAD_PROCESSES_ABSENT: &str = "confirm-workload-processes-absent";

/// Decide the one next action for `view`.
pub fn plan(view: &DaemonView) -> Plan {
    if let RegistryObservation::Unreadable { reason } = &view.registry {
        return Plan::Blocked {
            cause: BlockCause::Unreadable,
            reason: format!("the generation registry is unreadable: {reason}"),
        };
    }
    if let Some((generation, reason)) =
        view.generations
            .iter()
            .find_map(|generation| match &generation.lifecycle {
                GenerationLifecycle::Unreadable { reason } => Some((generation, reason)),
                _ => None,
            })
    {
        return Plan::Blocked {
            cause: BlockCause::Unreadable,
            reason: format!(
                "generation {} in {} is unreadable: {reason}",
                label(generation),
                generation.state_dir.display()
            ),
        };
    }

    // Dead generations first: their jobs decide whether anything may proceed.
    for generation in view.generations.iter().filter(|g| is_dead(g)) {
        if let Some(plan) = plan_dead_generation_jobs(generation) {
            return plan;
        }
    }

    let live: Vec<&GenerationView> = view.generations.iter().filter(|g| is_live(g)).collect();

    // A daemon process that holds no lease may own one of these stores. Any
    // start, claim, or stop could race it, so nothing proceeds.
    if live.is_empty() && !view.unleased_candidates.is_empty() {
        let pids = view
            .unleased_candidates
            .iter()
            .map(|candidate| candidate.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Plan::Blocked {
            cause: BlockCause::UnleasedProcess,
            reason: format!(
                "live daemon process(es) {pids} hold no lease; no process is signaled and no daemon is started while ownership is unresolved"
            ),
        };
    }

    if let Some(plan) = plan_admission(view, &live) {
        return plan;
    }

    for generation in &live {
        if let Some(plan) = plan_stale_live_generation(generation) {
            return plan;
        }
    }

    for generation in &view.generations {
        if let Some(step) = retirement(view, generation) {
            return Plan::Transition { step };
        }
    }

    Plan::Converged
}

fn plan_dead_generation_jobs(generation: &GenerationView) -> Option<Plan> {
    let lease_id = generation.lease_id.clone()?;
    if let Some(job) = generation
        .jobs
        .iter()
        .find(|job| job.custody == JobCustody::Running)
    {
        return Some(Plan::Wait {
            reason: format!(
                "job {} of dead generation {lease_id} still has a live workload process",
                job.job_id
            ),
        });
    }
    let unproven = generation
        .jobs
        .iter()
        .any(|job| job.custody == JobCustody::Orphaned { proof: None });
    if unproven {
        // The store accepts the attestation only for the complete active set
        // of the lease, so name every job still owned by it.
        return Some(Plan::NeedsAttestation(Attestation {
            lease_id,
            state_dir: generation.state_dir.clone(),
            job_ids: generation.jobs.iter().map(|job| job.job_id).collect(),
            confirmation: CONFIRM_WORKLOAD_PROCESSES_ABSENT,
        }));
    }
    let proven: Vec<Uuid> = generation
        .jobs
        .iter()
        .filter(|job| {
            matches!(
                job.custody,
                JobCustody::Orphaned { proof: Some(_) } | JobCustody::TerminalEvidence { .. }
            )
        })
        .map(|job| job.job_id)
        .collect();
    if !proven.is_empty() {
        return Some(Plan::Transition {
            step: Step::ReconcileProvenJobs {
                lease_id,
                job_ids: proven,
            },
        });
    }
    let resumable: Vec<Uuid> = generation
        .jobs
        .iter()
        .filter(|job| job.custody == JobCustody::Resumable)
        .map(|job| job.job_id)
        .collect();
    (!resumable.is_empty()).then_some(Plan::Transition {
        step: Step::ResumeDriverWork {
            lease_id,
            job_ids: resumable,
        },
    })
}

fn plan_admission(view: &DaemonView, live: &[&GenerationView]) -> Option<Plan> {
    let owner_lease = view.admission_owner.as_deref();
    let owner = owner_lease.and_then(|lease| {
        view.generations
            .iter()
            .find(|g| g.registered && g.lease_id.as_deref() == Some(lease))
    });
    let owner_serving = owner.is_some_and(is_live);
    if owner_serving {
        return None;
    }
    // A registry whose owner is dead, stopped, or missing entirely.
    if let Some(owner_lease) = owner_lease {
        let admitting = live
            .iter()
            .find(|g| g.lifecycle == GenerationLifecycle::Admitting);
        return Some(match admitting.and_then(|g| g.lease_id.clone()) {
            Some(to_lease_id) => Plan::Transition {
                step: Step::ClaimAdmission {
                    from_lease_id: owner_lease.to_string(),
                    to_lease_id,
                },
            },
            // Nothing admitting. Draining generations never take admission
            // back, so a new daemon must start; they retire on their own.
            None => Plan::Transition {
                step: Step::StartDaemon,
            },
        });
    }
    // No registry: a stale lease with nothing live means the daemon died.
    let dead_unregistered = view.generations.iter().any(|g| !g.registered && is_dead(g));
    (live.is_empty() && dead_unregistered).then_some(Plan::Transition {
        step: Step::StartDaemon,
    })
}

fn plan_stale_live_generation(generation: &GenerationView) -> Option<Plan> {
    // A draining generation is already on its way out; retirement owns it.
    if generation.lifecycle != GenerationLifecycle::Admitting {
        return None;
    }
    if !matches!(
        generation.binary,
        BinaryFreshness::Replaced | BinaryFreshness::Stale { .. }
    ) {
        return None;
    }
    let lease_id = generation.lease_id.clone()?;
    if !generation.jobs.is_empty() {
        return Some(Plan::Wait {
            reason: format!(
                "generation {lease_id} runs a stale binary but still owns {} job(s); it is replaced when they finish",
                generation.jobs.len()
            ),
        });
    }
    Some(Plan::Transition {
        step: match generation.supervision {
            Supervision::External => Step::StopViaLifecycleEndpoint { lease_id },
            Supervision::HomeboySupervised => Step::StopByLease { lease_id },
            // No lease credential to prove a lease-bound stop is safe.
            Supervision::Unknown => {
                return Some(Plan::Blocked {
                    cause: BlockCause::UnknownSupervision,
                    reason: format!(
                        "generation {lease_id} runs a stale binary but its supervision is unknown"
                    ),
                })
            }
        },
    })
}

fn retirement(view: &DaemonView, generation: &GenerationView) -> Option<Step> {
    if !generation.registered || !generation.jobs.is_empty() {
        return None;
    }
    let lease_id = generation.lease_id.clone()?;
    if view.admission_owner.as_deref() == Some(lease_id.as_str()) {
        return None;
    }
    let retirable = match generation.lifecycle {
        GenerationLifecycle::Dead { .. } | GenerationLifecycle::Stopped => true,
        GenerationLifecycle::Draining => true,
        GenerationLifecycle::Admitting | GenerationLifecycle::Unreadable { .. } => false,
    };
    retirable.then_some(Step::RetireGeneration { lease_id })
}

fn is_live(generation: &GenerationView) -> bool {
    matches!(
        generation.lifecycle,
        GenerationLifecycle::Admitting | GenerationLifecycle::Draining
    )
}

fn is_dead(generation: &GenerationView) -> bool {
    matches!(generation.lifecycle, GenerationLifecycle::Dead { .. })
}

fn label(generation: &GenerationView) -> &str {
    generation.lease_id.as_deref().unwrap_or("<no lease>")
}

#[cfg(test)]
#[path = "lifecycle_plan_tests.rs"]
mod tests;
