use super::*;

#[test]
fn reserved_gate_ignores_bulk_pressure_but_waits_for_slots_and_memory() {
    crate::test_support::with_isolated_home(|home| {
        if crate::lab_capacity::allowed_cpus().len() < 5 {
            return;
        }
        std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "4");
        std::env::set_var("HOMEBOY_LAB_GATE_SLOTS", "2");
        std::env::set_var(
            crate::runner_pressure::SAMPLE_ENV,
            r#"{"cpu":9000,"io":9000,"memory":0,"observed_load_per_cpu":90000}"#,
        );
        let store = JobStore::open_without_reconciliation(
            &home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap();
        let gate = store.create("runner.exec");
        let second = store.create("runner.exec");
        let third = store.create("runner.exec");
        let step = store.create("workflow.step");
        store
            .durable_transaction(|inner| {
                for id in [gate.id, second.id, third.id] {
                    inner.jobs.get_mut(&id).unwrap().job.runner_job_projection =
                        Some(crate::api_jobs::RunnerJobProjection {
                            runner_id: "lab".into(),
                            command: "true".into(),
                            cwd: None,
                            source: "test".into(),
                            kind: "runner.exec".into(),
                            lifecycle: None,
                        });
                }
                Ok(())
            })
            .unwrap();
        assert!(!store.reconcile_direct_admission(step.id).unwrap());
        assert!(store.reconcile_direct_admission(gate.id).unwrap());
        assert!(store
            .reserve_local_child_with_runner_capacity(gate.id, "lab", 1)
            .unwrap());
        assert!(store.reconcile_direct_admission(second.id).unwrap());
        assert!(store
            .reserve_local_child_with_runner_capacity(second.id, "lab", 1)
            .unwrap());
        assert_eq!(store.local_child_gate_slot(gate.id).unwrap(), Some(0));
        assert_eq!(store.local_child_gate_slot(second.id).unwrap(), Some(1));
        let capacity = store.gate_capacity();
        assert_eq!(capacity["slots"], 2);
        let first_cpus = capacity["active"][0]["cpus"].as_array().unwrap();
        let second_cpus = capacity["active"][1]["cpus"].as_array().unwrap();
        assert_eq!(first_cpus.len(), 2);
        assert_eq!(second_cpus.len(), 2);
        assert!(first_cpus.iter().all(|cpu| !second_cpus.contains(cpu)));
        // Reopening the durable store preserves both occupied indices.
        let reopened = JobStore::open_without_reconciliation(
            &home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap();
        assert_eq!(reopened.gate_capacity(), capacity);
        assert!(!store.reconcile_direct_admission(third.id).unwrap());
        assert_eq!(
            store.pending_admission_report(third.id).unwrap()["pending_admission_reason"],
            "gate_slot"
        );
        assert!(!store
            .reserve_local_child_with_runner_capacity(third.id, "lab", 1)
            .unwrap());
        assert_eq!(store.gate_capacity()["active"].as_array().unwrap().len(), 2);
        assert_eq!(store.gate_capacity()["queued_slots"], 1);
        store.fail(gate.id, "test finished").unwrap();
        assert!(store.reconcile_direct_admission(third.id).unwrap());
        assert!(store
            .reserve_local_child_with_runner_capacity(third.id, "lab", 1)
            .unwrap());
        assert_eq!(store.local_child_gate_slot(third.id).unwrap(), Some(0));
        std::env::set_var(
            crate::runner_pressure::SAMPLE_ENV,
            r#"{"cpu":9000,"memory":9000}"#,
        );
        assert!(!store.reconcile_direct_admission(second.id).unwrap());
        std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "0");
        std::env::set_var(crate::runner_pressure::SAMPLE_ENV, r#"{"cpu":9000}"#);
        assert!(!store.reconcile_direct_admission(second.id).unwrap());
        std::env::remove_var("HOMEBOY_LAB_GATE_RESERVE");
        std::env::remove_var("HOMEBOY_LAB_GATE_SLOTS");
        std::env::remove_var(crate::runner_pressure::SAMPLE_ENV);
    });
}
