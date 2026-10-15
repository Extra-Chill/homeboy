//! Cadence slot math, registration semantics, and the tick against a real job
//! store with real `step` children: a slot fires once, a running previous
//! instance skips the slot, and re-firing a slot after a crash converges on
//! the instance it already admitted.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::api_jobs::JobStore;
use crate::daemon::workflow_job::{register_workflow_job_driver, WORKFLOW_JOB_REQUEST_SCHEMA};
use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::test_support::with_isolated_home;

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;

fn template(every: Value, script: &str, cwd: &Path) -> WorkflowJobRequest {
    let definition = WorkflowDefinition::from_value(json!({
        "schema": "homeboy/workflow/v1",
        "id": "tick",
        "every": every,
        "steps": [{"id": "run", "exec": {"program": "sh", "args": ["-c", script]}}]
    }))
    .unwrap();
    definition.validate(&|_| true).unwrap();
    let inputs = definition.resolve_inputs(&BTreeMap::new()).unwrap();
    let mut env = EnvMaterializationPlan::default();
    env.public_env
        .insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    WorkflowJobRequest {
        schema: WORKFLOW_JOB_REQUEST_SCHEMA.to_string(),
        instance: definition.instance_id(&inputs, None),
        instance_name: None,
        definition_digest: definition.digest(),
        definition,
        inputs,
        homeboy_program: "/bin/sh".to_string(),
        cwd: cwd.display().to_string(),
        env,
        homeboy_env: EnvMaterializationPlan::default(),
        programs: BTreeMap::from([("sh".to_string(), "/bin/sh".to_string())]),
    }
}

fn cadence(anchor_ms: u64, interval_ms: u64, next_due_ms: u64) -> Cadence {
    with_isolated_home(|home| {
        let request = template(json!({"interval": "1h"}), "true", home.path());
        Cadence {
            schema: CADENCE_SCHEMA.to_string(),
            workflow: "tick".to_string(),
            request,
            builtin: false,
            registered_at_ms: anchor_ms,
            anchor_ms,
            interval_ms,
            next_due_ms,
            cancelled_at_ms: None,
            last_instance: None,
            last_instance_store: None,
            pending: None,
            last_fired_at_ms: None,
            fired: 0,
            skipped: 0,
            last_skipped_at_ms: None,
            last_error: None,
        }
    })
}

#[test]
fn an_at_anchor_puts_slots_on_the_utc_clock() {
    let definition = WorkflowDefinition::from_value(json!({
        "schema": "homeboy/workflow/v1", "id": "nightly",
        "every": {"interval": "24h", "at": "03:00Z"},
        "steps": [{"id": "a", "exec": {"program": "true"}}]
    }))
    .unwrap();
    // 2026-10-13T15:20:00Z
    let now = 1_791_904_800_000;
    let (anchor, interval) = grid(&definition, now).unwrap();
    assert_eq!((anchor, interval), (3 * HOUR, 24 * HOUR));
    let due = first_due(anchor, interval, now);
    assert_eq!(slot_instance_name(due), "every-2026-10-14T03:00:00Z");
}

#[test]
fn without_an_anchor_the_first_slot_is_registration() {
    assert_eq!(first_due(1_000, HOUR, 1_000), 1_000);
    assert_eq!(first_due(1_000, HOUR, 1_001), 1_000 + HOUR);
}

#[test]
fn decide_fires_once_per_slot_coalesces_missed_slots_and_skips_overlap() {
    let base = 1_791_900_000_000;
    let due = cadence(base, 2 * MINUTE, base);
    assert_eq!(decide(&due, base - 1, false), Decision::NotDue);
    assert_eq!(
        decide(&due, base + 30_000, false),
        Decision::Fire {
            slot_ms: base,
            instance_name: slot_instance_name(base),
            next_due_ms: base + 2 * MINUTE,
        }
    );
    // Three slots passed while the daemon was down: one instance, for the latest.
    assert_eq!(
        decide(&due, base + 7 * MINUTE, false),
        Decision::Fire {
            slot_ms: base + 6 * MINUTE,
            instance_name: slot_instance_name(base + 6 * MINUTE),
            next_due_ms: base + 8 * MINUTE,
        }
    );
    assert_eq!(
        decide(&due, base + 30_000, true),
        Decision::Skip {
            slot_ms: base,
            next_due_ms: base + 2 * MINUTE
        }
    );
    let mut cancelled = due;
    cancelled.cancelled_at_ms = Some(base);
    assert_eq!(decide(&cancelled, base + HOUR, false), Decision::NotDue);
}

#[test]
fn a_slot_names_a_deterministic_instance() {
    with_isolated_home(|home| {
        let template = template(json!({"interval": "1h"}), "true", home.path());
        let first = slot_request(&template, "every-2026-10-13T15:00:00Z");
        let again = slot_request(&template, "every-2026-10-13T15:00:00Z");
        let next = slot_request(&template, "every-2026-10-13T16:00:00Z");
        assert_eq!(first.instance, again.instance);
        assert_ne!(first.instance, next.instance);
        assert!(first.instance.starts_with("wf-tick-"));
        WorkflowJobRequest::parse(&serde_json::to_value(&first).unwrap()).unwrap();
    });
}

#[test]
fn a_builtin_first_fires_one_interval_after_the_daemon_installs_it() {
    with_isolated_home(|home| {
        let hourly = template(json!({"interval": "1h"}), "true", home.path());
        let (installed, disposition) = register(hourly, true, 1_000).unwrap();
        assert_eq!(disposition, "registered");
        assert_eq!(installed.next_due_ms, 1_000 + HOUR);
    });
}

#[test]
fn registration_keeps_its_grid_and_a_builtin_never_overrides_the_operator() {
    with_isolated_home(|home| {
        let hourly = template(json!({"interval": "1h"}), "true", home.path());
        let (registered, disposition) = register(hourly.clone(), false, 1_000).unwrap();
        assert_eq!(disposition, "registered");
        assert_eq!(registered.next_due_ms, 1_000);

        let (same, disposition) = register(hourly.clone(), false, 5_000).unwrap();
        assert_eq!(disposition, "unchanged");
        assert_eq!(same.anchor_ms, 1_000);

        let two_hourly = template(json!({"interval": "2h"}), "true", home.path());
        let (changed, disposition) = register(two_hourly, false, 9_000).unwrap();
        assert_eq!(disposition, "updated");
        assert_eq!((changed.anchor_ms, changed.interval_ms), (9_000, 2 * HOUR));

        // A built-in with the same id keeps the operator's registration.
        let (kept, disposition) = register(hourly.clone(), true, 10_000).unwrap();
        assert_eq!(disposition, "kept");
        assert_eq!(kept.interval_ms, 2 * HOUR);

        // An opt-out survives the daemon's built-in registration.
        let cancelled = cancel("tick", 11_000).unwrap();
        assert_eq!(cancelled.cancelled_at_ms, Some(11_000));
        let (kept, disposition) = register(hourly.clone(), true, 12_000).unwrap();
        assert_eq!(disposition, "kept");
        assert!(kept.cancelled_at_ms.is_some());

        // The operator re-registering resumes it.
        let (resumed, disposition) = register(hourly, false, 13_000).unwrap();
        assert_eq!(disposition, "updated");
        assert_eq!(resumed.cancelled_at_ms, None);
        assert_eq!(list().unwrap().len(), 1);
        assert!(cancel("missing", 0).is_err());
    });
}

fn wait_terminal(store: &JobStore, instance: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if store
            .controller_job_for_key(instance)
            .is_some_and(|job| job.status.is_terminal())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "instance {instance} did not finish"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The tick against a real store: slot 1 fires, slot 2 is skipped while slot
/// 1's instance runs, slot 3 fires after it finishes, and re-firing slot 3
/// (a crash before the registration advanced) reuses its instance.
#[test]
fn the_tick_fires_skips_overlap_and_never_fires_a_slot_twice() {
    with_isolated_home(|home| {
        register_workflow_job_driver();
        let gate = home.path().join("release");
        let runs = home.path().join("runs.log");
        let script = format!(
            "echo run >> {runs}; while [ ! -f {gate} ]; do sleep 0.05; done",
            runs = runs.display(),
            gate = gate.display()
        );
        let store = JobStore::open_without_reconciliation(home.path().join("jobs.json"))
            .unwrap()
            .with_daemon_lease("cadence-test".to_string());
        let base = 1_791_900_000_000;
        register(
            template(json!({"interval": "2m"}), &script, home.path()),
            false,
            base,
        )
        .unwrap();

        let fired = admit_due(&store, base + 1_000);
        assert_eq!(fired.len(), 1, "{fired:?}");
        let first = list().unwrap()[0].last_instance.clone().unwrap();
        assert!(admit_due(&store, base + 2_000).is_empty(), "not due again");

        // Slot 2 comes due while slot 1's instance is still running.
        let skipped = admit_due(&store, base + 2 * MINUTE + 1_000);
        assert_eq!(skipped[0]["skipped"], base + 2 * MINUTE);
        let after_skip = list().unwrap().remove(0);
        assert_eq!((after_skip.fired, after_skip.skipped), (1, 1));
        assert_eq!(after_skip.next_due_ms, base + 4 * MINUTE);

        std::fs::write(&gate, "").unwrap();
        wait_terminal(&store, &first);

        let third = admit_due(&store, base + 4 * MINUTE + 1_000);
        assert_eq!(third.len(), 1);
        let registration = list().unwrap().remove(0);
        let second = registration.last_instance.clone().unwrap();
        assert_ne!(first, second);
        wait_terminal(&store, &second);

        // Crash after admission, before the registration advanced: the slot
        // is still pending and is completed, not admitted again.
        let mut rewound = registration.clone();
        rewound.next_due_ms = base + 4 * MINUTE;
        rewound.pending = Some(PendingSlot {
            slot_ms: base + 4 * MINUTE,
            instance_name: slot_instance_name(base + 4 * MINUTE),
            instance: second.clone(),
            store: store.persistence_path(),
            next_due_ms: base + 6 * MINUTE,
        });
        write(&open().unwrap(), &rewound).unwrap();
        let replayed = admit_due(&store, base + 4 * MINUTE + 2_000);
        assert_eq!(replayed[0]["instance"], second);
        assert_eq!(replayed[0]["admitted"], "existing");
        let completed = list().unwrap().remove(0);
        assert_eq!(completed.pending, None);
        assert_eq!(completed.next_due_ms, base + 6 * MINUTE);
        assert_eq!(
            store.controller_jobs_of_type("workflow").len(),
            2,
            "two slots fired, one skipped, none twice"
        );
        let lines = std::fs::read_to_string(&runs).unwrap();
        assert_eq!(lines.lines().count(), 2);
    });
}
