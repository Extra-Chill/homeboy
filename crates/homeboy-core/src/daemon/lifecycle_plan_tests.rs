//! Planner scenarios and exhaustive invariants (#15557, slice C2).

use super::*;
use crate::api_jobs::DaemonActiveJobRecoveryDisposition as Disposition;
use crate::daemon::lifecycle::{
    DeathProof, JobCustodyView, Liveness, ProcessObservation, UnleasedCandidate,
};
use crate::daemon::DaemonProcessOwnership;
use std::path::PathBuf;

fn job(n: u128, custody: JobCustody) -> JobCustodyView {
    let disposition = match &custody {
        JobCustody::Running => Disposition::ProtectedLive,
        JobCustody::Resumable => Disposition::DriverRecovery,
        JobCustody::TerminalEvidence { .. } => Disposition::TerminalEvidence,
        JobCustody::Orphaned { proof: Some(_) } => Disposition::DeadChild,
        JobCustody::Orphaned { proof: None } => Disposition::BlockingAmbiguous,
    };
    JobCustodyView {
        job_id: Uuid::from_u128(n),
        operation: "runner.exec".to_string(),
        disposition,
        custody,
    }
}

fn generation(lease: &str, lifecycle: GenerationLifecycle) -> GenerationView {
    let live = lifecycle == GenerationLifecycle::Admitting;
    GenerationView {
        lease_id: Some(lease.to_string()),
        state_dir: PathBuf::from("/daemon"),
        lifecycle,
        process: ProcessObservation {
            pid: Some(100),
            liveness: if live { Liveness::Live } else { Liveness::Dead },
        },
        supervision: Supervision::HomeboySupervised,
        binary: if live {
            BinaryFreshness::Current
        } else {
            BinaryFreshness::Unknown
        },
        jobs: Vec::new(),
    }
}

fn dead() -> GenerationLifecycle {
    GenerationLifecycle::Dead {
        proof: DeathProof::PidDead,
    }
}

fn view(generations: Vec<GenerationView>) -> DaemonView {
    DaemonView {
        state_dir: PathBuf::from("/daemon"),
        generations,
        unleased_candidates: Vec::new(),
    }
}

fn step(plan: &Plan) -> &Step {
    match plan {
        Plan::Transition { step } => step,
        other => panic!("expected a transition, got {other:?}"),
    }
}

#[test]
fn a_healthy_admitting_daemon_is_converged() {
    let v = view(vec![generation("LIVE", GenerationLifecycle::Admitting)]);
    assert_eq!(plan(&v), Plan::Converged);
}

#[test]
fn no_daemon_at_all_is_converged() {
    assert_eq!(plan(&view(Vec::new())), Plan::Converged);
}

/// Chaos case 2 (#15556): uncheckpointed work of a killed daemon needs exactly
/// one attestation over the complete job set.
#[test]
fn unproven_orphans_need_one_attestation_over_the_whole_job_set() {
    let mut killed = generation("KILLED", dead());
    killed.jobs = vec![
        job(1, JobCustody::Orphaned { proof: None }),
        job(
            2,
            JobCustody::Orphaned {
                proof: Some(DeathProof::PidDead),
            },
        ),
    ];
    let v = view(vec![killed]);
    assert_eq!(
        plan(&v),
        Plan::NeedsAttestation(Attestation {
            lease_id: "KILLED".to_string(),
            state_dir: PathBuf::from("/daemon"),
            job_ids: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
            confirmation: CONFIRM_WORKLOAD_PROCESSES_ABSENT.to_string(),
        })
    );
}

/// Chaos case 8: a dead daemon's proven-dead job reconciles, then the empty
/// directory restarts.
#[test]
fn a_dead_daemon_reconciles_proven_jobs_then_restarts() {
    let mut old = generation("OLD", dead());
    old.jobs = vec![job(
        3,
        JobCustody::Orphaned {
            proof: Some(DeathProof::PidDead),
        },
    )];
    assert_eq!(
        step(&plan(&view(vec![old.clone()]))),
        &Step::ReconcileProvenJobs {
            lease_id: "OLD".to_string(),
            job_ids: vec![Uuid::from_u128(3)],
        }
    );
    old.jobs.clear();
    assert_eq!(step(&plan(&view(vec![old]))), &Step::StartDaemon);
}

/// #15420/#15426: checkpointed driver work resumes without attestation.
#[test]
fn checkpointed_driver_work_resumes_without_attestation() {
    let mut killed = generation("KILLED", dead());
    killed.jobs = vec![job(4, JobCustody::Resumable)];
    let v = view(vec![killed]);
    assert_eq!(
        step(&plan(&v)),
        &Step::ResumeDriverWork {
            lease_id: "KILLED".to_string(),
            job_ids: vec![Uuid::from_u128(4)],
        }
    );
}

#[test]
fn a_live_child_of_a_dead_generation_is_waited_for() {
    let mut killed = generation("KILLED", dead());
    killed.jobs = vec![
        job(5, JobCustody::Running),
        job(6, JobCustody::Orphaned { proof: None }),
    ];
    let v = view(vec![killed]);
    assert!(matches!(plan(&v), Plan::Wait { .. }));
}

/// Chaos case 9: the whole process group died, nothing is left serving.
#[test]
fn a_dead_owner_with_nothing_live_starts_a_daemon() {
    let mut killed = generation("KILLED", dead());
    killed.process.liveness = Liveness::Zombie;
    let v = view(vec![killed]);
    assert_eq!(step(&plan(&v)), &Step::StartDaemon);
}

/// Chaos case 5 (#15437): a stale `daemon serve` is stopped through its
/// lifecycle endpoint, never by PID.
#[test]
fn a_stale_external_daemon_stops_through_its_lifecycle_endpoint() {
    let mut serve = generation("SERVE", GenerationLifecycle::Admitting);
    serve.supervision = Supervision::External;
    serve.binary = BinaryFreshness::Replaced;
    assert_eq!(
        step(&plan(&view(vec![serve]))),
        &Step::StopViaLifecycleEndpoint {
            lease_id: "SERVE".to_string()
        }
    );
}

/// Chaos cases 3/4: a replaced binary under an idle daemon converges; under a
/// busy one it waits for the work.
#[test]
fn a_replaced_binary_stops_when_idle_and_waits_when_busy() {
    let mut idle = generation("UPGRADED", GenerationLifecycle::Admitting);
    idle.binary = BinaryFreshness::Replaced;
    assert_eq!(
        step(&plan(&view(vec![idle.clone()]))),
        &Step::StopByLease {
            lease_id: "UPGRADED".to_string()
        }
    );
    let mut busy = idle;
    busy.jobs = vec![job(7, JobCustody::Running)];
    assert!(matches!(plan(&view(vec![busy])), Plan::Wait { .. }));
}

#[test]
fn unreadable_state_blocks_instead_of_guessing() {
    let v = view(vec![generation(
        "X",
        GenerationLifecycle::Unreadable {
            reason: "lease is corrupt".to_string(),
        },
    )]);
    assert!(
        matches!(plan(&v), Plan::Blocked { cause: BlockCause::Unreadable, reason } if reason.contains("lease is corrupt"))
    );
}

#[test]
fn an_unleased_live_daemon_blocks_any_start() {
    let mut v = view(vec![generation("KILLED", dead())]);
    v.unleased_candidates = vec![UnleasedCandidate {
        pid: 4242,
        ownership: DaemonProcessOwnership::Ambiguous,
        bind_endpoint: None,
    }];
    assert!(
        matches!(plan(&v), Plan::Blocked { cause: BlockCause::UnleasedProcess, reason } if reason.contains("4242"))
    );
}

// ---------------------------------------------------------------------------
// Exhaustive invariants over every lifecycle × supervision × binary × custody
// combination.
// ---------------------------------------------------------------------------

fn lifecycles() -> Vec<GenerationLifecycle> {
    vec![
        GenerationLifecycle::Admitting,
        GenerationLifecycle::Stopped,
        dead(),
        GenerationLifecycle::Dead {
            proof: DeathProof::TerminationEvidence,
        },
    ]
}

fn custody_sets() -> Vec<Vec<JobCustodyView>> {
    let all = [
        JobCustody::Running,
        JobCustody::Resumable,
        JobCustody::TerminalEvidence { status: None },
        JobCustody::Orphaned {
            proof: Some(DeathProof::PidDead),
        },
        JobCustody::Orphaned { proof: None },
    ];
    let mut sets = vec![Vec::new()];
    for (index, custody) in all.iter().enumerate() {
        sets.push(vec![job(index as u128 + 10, custody.clone())]);
    }
    // Live work beside an unproven orphan: the live child must win.
    sets.push(vec![
        job(20, JobCustody::Orphaned { proof: None }),
        job(21, JobCustody::Running),
    ]);
    sets
}

fn all_views() -> Vec<DaemonView> {
    let mut views = Vec::new();
    for lifecycle in lifecycles() {
        for supervision in [
            Supervision::HomeboySupervised,
            Supervision::External,
            Supervision::Unknown,
        ] {
            for binary in [
                BinaryFreshness::Current,
                BinaryFreshness::Replaced,
                BinaryFreshness::Stale { reason_code: None },
            ] {
                for jobs in custody_sets() {
                    let mut target = generation("T", lifecycle.clone());
                    target.supervision = supervision;
                    target.binary = binary;
                    target.jobs = jobs;
                    views.push(view(vec![target]));
                }
            }
        }
    }
    views
}

fn step_lease(step: &Step) -> Option<&str> {
    match step {
        Step::ReconcileProvenJobs { lease_id, .. }
        | Step::ResumeDriverWork { lease_id, .. }
        | Step::StopByLease { lease_id }
        | Step::StopViaLifecycleEndpoint { lease_id } => Some(lease_id),
        Step::StartDaemon => None,
    }
}

#[test]
fn every_plan_satisfies_the_planner_invariants() {
    let views = all_views();
    assert!(views.len() > 200, "the table covers {} views", views.len());
    for v in &views {
        let result = plan(v);
        let context = || format!("view: {v:#?}\nplan: {result:?}");
        assert_eq!(plan(v), result, "plan is deterministic\n{}", context());

        let generation_of = |lease: &str| {
            v.generations
                .iter()
                .find(|g| g.lease_id.as_deref() == Some(lease))
                .unwrap_or_else(|| panic!("step names an unknown lease {lease}\n{}", context()))
        };
        match &result {
            Plan::Transition { step } => {
                if let Some(lease) = step_lease(step) {
                    let target = generation_of(lease);
                    assert!(
                        target.jobs.iter().all(|j| j.custody != JobCustody::Running),
                        "a transition never touches a generation with live work\n{}",
                        context()
                    );
                    if !matches!(step, Step::ReconcileProvenJobs { .. }) {
                        assert!(
                            !target
                                .jobs
                                .iter()
                                .any(|j| j.custody == JobCustody::Orphaned { proof: None }),
                            "unproven orphans are never bypassed\n{}",
                            context()
                        );
                    }
                }
                match step {
                    Step::StopByLease { lease_id } => assert_eq!(
                        generation_of(lease_id).supervision,
                        Supervision::HomeboySupervised,
                        "only supervised daemons are stopped by lease\n{}",
                        context()
                    ),
                    Step::StopViaLifecycleEndpoint { lease_id } => assert_eq!(
                        generation_of(lease_id).supervision,
                        Supervision::External,
                        "{}",
                        context()
                    ),
                    Step::StartDaemon => assert!(
                        !v.generations
                            .iter()
                            .any(|g| g.lifecycle == GenerationLifecycle::Admitting),
                        "never start beside a live admitting daemon\n{}",
                        context()
                    ),
                    _ => {}
                }
            }
            Plan::NeedsAttestation(attestation) => {
                let target = generation_of(&attestation.lease_id);
                assert!(
                    matches!(target.lifecycle, GenerationLifecycle::Dead { .. }),
                    "only dead generations are attested\n{}",
                    context()
                );
                assert!(
                    target
                        .jobs
                        .iter()
                        .any(|j| j.custody == JobCustody::Orphaned { proof: None }),
                    "attestation only for unproven orphans\n{}",
                    context()
                );
                assert!(
                    target.jobs.iter().all(|j| j.custody != JobCustody::Running),
                    "never attest beside live work\n{}",
                    context()
                );
                assert_eq!(
                    attestation.job_ids,
                    target.jobs.iter().map(|j| j.job_id).collect::<Vec<_>>(),
                    "the attestation names the complete job set\n{}",
                    context()
                );
            }
            Plan::Converged => {
                for g in &v.generations {
                    if matches!(g.lifecycle, GenerationLifecycle::Dead { .. }) {
                        assert!(
                            g.jobs.is_empty(),
                            "converged never leaves a dead generation's jobs\n{}",
                            context()
                        );
                    }
                }
            }
            Plan::Wait { .. } | Plan::Blocked { .. } => {}
        }
    }
}

#[test]
fn plans_serialize_with_stable_tags() {
    assert_eq!(
        serde_json::to_value(Plan::Transition {
            step: Step::StartDaemon
        })
        .unwrap(),
        serde_json::json!({ "plan": "transition", "step": { "step": "start_daemon" } })
    );
    assert_eq!(
        serde_json::to_value(Plan::Converged).unwrap(),
        serde_json::json!({ "plan": "converged" })
    );
}
