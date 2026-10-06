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
    let live = matches!(
        lifecycle,
        GenerationLifecycle::Admitting | GenerationLifecycle::Draining
    );
    GenerationView {
        lease_id: Some(lease.to_string()),
        state_dir: PathBuf::from(format!("/daemon/{lease}")),
        registered: true,
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

fn view(admission_owner: Option<&str>, generations: Vec<GenerationView>) -> DaemonView {
    DaemonView {
        router_dir: PathBuf::from("/daemon"),
        registry: if admission_owner.is_some() {
            RegistryObservation::Present
        } else {
            RegistryObservation::Absent
        },
        admission_owner: admission_owner.map(str::to_string),
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
    let v = view(
        Some("LIVE"),
        vec![generation("LIVE", GenerationLifecycle::Admitting)],
    );
    assert_eq!(plan(&v), Plan::Converged);
}

#[test]
fn no_daemon_at_all_is_converged() {
    assert_eq!(plan(&view(None, Vec::new())), Plan::Converged);
}

/// Chaos case 7 (#15456): the registry still routes to the dead lease while
/// its directory serves the in-place restart.
#[test]
fn in_place_restart_claims_admission_for_the_live_lease() {
    let mut restarted = generation("LIVE", GenerationLifecycle::Admitting);
    restarted.registered = false;
    let v = view(
        Some("DEAD"),
        vec![
            generation(
                "DEAD",
                GenerationLifecycle::Dead {
                    proof: DeathProof::SameDirReplaced {
                        current_lease_id: "LIVE".to_string(),
                    },
                },
            ),
            restarted,
        ],
    );
    assert_eq!(
        step(&plan(&v)),
        &Step::ClaimAdmission {
            from_lease_id: "DEAD".to_string(),
            to_lease_id: "LIVE".to_string(),
        }
    );
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
    let v = view(Some("KILLED"), vec![killed]);
    assert_eq!(
        plan(&v),
        Plan::NeedsAttestation(Attestation {
            lease_id: "KILLED".to_string(),
            state_dir: PathBuf::from("/daemon/KILLED"),
            job_ids: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
            confirmation: CONFIRM_WORKLOAD_PROCESSES_ABSENT,
        })
    );
}

/// Chaos case 8: a dead draining generation's proven-dead job reconciles,
/// then the empty generation retires.
#[test]
fn dead_draining_generation_reconciles_proven_jobs_then_retires() {
    let mut old = generation("OLD", dead());
    old.jobs = vec![job(
        3,
        JobCustody::Orphaned {
            proof: Some(DeathProof::PidDead),
        },
    )];
    let live = generation("LIVE", GenerationLifecycle::Admitting);
    let v = view(Some("LIVE"), vec![live.clone(), old.clone()]);
    assert_eq!(
        step(&plan(&v)),
        &Step::ReconcileProvenJobs {
            lease_id: "OLD".to_string(),
            job_ids: vec![Uuid::from_u128(3)],
        }
    );
    old.jobs.clear();
    let after = view(Some("LIVE"), vec![live, old]);
    assert_eq!(
        step(&plan(&after)),
        &Step::RetireGeneration {
            lease_id: "OLD".to_string()
        }
    );
}

/// #15420/#15426: checkpointed driver work resumes without attestation.
#[test]
fn checkpointed_driver_work_resumes_without_attestation() {
    let mut killed = generation("KILLED", dead());
    killed.jobs = vec![job(4, JobCustody::Resumable)];
    let v = view(Some("KILLED"), vec![killed]);
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
    let v = view(Some("KILLED"), vec![killed]);
    assert!(matches!(plan(&v), Plan::Wait { .. }));
}

/// Chaos case 9: the whole process group died, nothing is left serving.
#[test]
fn a_dead_owner_with_nothing_live_starts_a_daemon() {
    let mut killed = generation("KILLED", dead());
    killed.process.liveness = Liveness::Zombie;
    let v = view(Some("KILLED"), vec![killed]);
    assert_eq!(step(&plan(&v)), &Step::StartDaemon);
}

#[test]
fn a_dead_owner_beside_only_draining_generations_starts_a_daemon() {
    let v = view(
        Some("KILLED"),
        vec![
            generation("KILLED", dead()),
            generation("OLD", GenerationLifecycle::Draining),
        ],
    );
    assert_eq!(step(&plan(&v)), &Step::StartDaemon);
}

#[test]
fn an_unregistered_dead_lease_restarts() {
    let mut legacy = generation("LEGACY", dead());
    legacy.registered = false;
    assert_eq!(step(&plan(&view(None, vec![legacy]))), &Step::StartDaemon);
}

/// Chaos case 5 (#15437): a stale `daemon serve` is stopped through its
/// lifecycle endpoint, never by PID.
#[test]
fn a_stale_external_daemon_stops_through_its_lifecycle_endpoint() {
    let mut serve = generation("SERVE", GenerationLifecycle::Admitting);
    serve.supervision = Supervision::External;
    serve.binary = BinaryFreshness::Replaced;
    assert_eq!(
        step(&plan(&view(Some("SERVE"), vec![serve]))),
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
        step(&plan(&view(Some("UPGRADED"), vec![idle.clone()]))),
        &Step::StopByLease {
            lease_id: "UPGRADED".to_string()
        }
    );
    let mut busy = idle;
    busy.jobs = vec![job(7, JobCustody::Running)];
    assert!(matches!(
        plan(&view(Some("UPGRADED"), vec![busy])),
        Plan::Wait { .. }
    ));
}

#[test]
fn unreadable_state_blocks_instead_of_guessing() {
    let v = view(
        Some("X"),
        vec![generation(
            "X",
            GenerationLifecycle::Unreadable {
                reason: "lease is corrupt".to_string(),
            },
        )],
    );
    assert!(matches!(plan(&v), Plan::Blocked { reason } if reason.contains("lease is corrupt")));
    let mut unreadable_registry = view(None, Vec::new());
    unreadable_registry.registry = RegistryObservation::Unreadable {
        reason: "truncated".to_string(),
    };
    assert!(matches!(plan(&unreadable_registry), Plan::Blocked { .. }));
}

#[test]
fn an_unleased_live_daemon_blocks_any_start() {
    let mut v = view(Some("KILLED"), vec![generation("KILLED", dead())]);
    v.unleased_candidates = vec![UnleasedCandidate {
        pid: 4242,
        ownership: DaemonProcessOwnership::Ambiguous,
        bind_endpoint: None,
    }];
    assert!(matches!(plan(&v), Plan::Blocked { reason } if reason.contains("4242")));
}

// ---------------------------------------------------------------------------
// Exhaustive invariants over every lifecycle × supervision × binary × custody
// × admission combination, alone and beside a healthy admitting generation.
// ---------------------------------------------------------------------------

fn lifecycles() -> Vec<GenerationLifecycle> {
    vec![
        GenerationLifecycle::Admitting,
        GenerationLifecycle::Draining,
        GenerationLifecycle::Stopped,
        dead(),
        GenerationLifecycle::Dead {
            proof: DeathProof::SameDirReplaced {
                current_lease_id: "OTHER".to_string(),
            },
        },
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
                    for owner in [Some("T"), Some("H"), None] {
                        for with_healthy in [false, true] {
                            let mut generations = vec![target.clone()];
                            if with_healthy {
                                generations.push(generation("H", GenerationLifecycle::Admitting));
                            }
                            views.push(view(owner, generations));
                        }
                    }
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
        | Step::StopViaLifecycleEndpoint { lease_id }
        | Step::RetireGeneration { lease_id } => Some(lease_id),
        // The old owner may be missing from the view entirely; the new owner
        // is checked separately below.
        Step::ClaimAdmission { .. } | Step::StartDaemon => None,
    }
}

#[test]
fn every_plan_satisfies_the_planner_invariants() {
    let views = all_views();
    assert!(views.len() > 1000, "the table covers {} views", views.len());
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
                    Step::ClaimAdmission { to_lease_id, .. } => assert_eq!(
                        generation_of(to_lease_id).lifecycle,
                        GenerationLifecycle::Admitting,
                        "admission only moves to a live admitting generation\n{}",
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
                if let Some(owner) = v.admission_owner.as_deref() {
                    assert!(
                        v.generations
                            .iter()
                            .any(|g| g.lease_id.as_deref() == Some(owner)
                                && g.lifecycle == GenerationLifecycle::Admitting
                                || g.lease_id.as_deref() == Some(owner)
                                    && g.lifecycle == GenerationLifecycle::Draining),
                        "converged means the admission owner serves\n{}",
                        context()
                    );
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
