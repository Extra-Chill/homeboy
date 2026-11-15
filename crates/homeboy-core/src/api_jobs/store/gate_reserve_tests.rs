use super::*;

#[test]
fn reserved_gate_ignores_bulk_pressure_but_waits_for_slots_and_memory() {
    crate::test_support::with_isolated_home(|home| {
        if crate::lab_capacity::allowed_cpus().len() < 2 {
            return;
        }
        std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "1");
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
        let step = store.create("workflow.step");
        store
            .durable_transaction(|inner| {
                for id in [gate.id, second.id] {
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
        assert!(!store.reconcile_direct_admission(second.id).unwrap());
        assert_eq!(
            store.pending_admission_report(second.id).unwrap()["pending_admission_reason"],
            "gate_slot"
        );
        assert!(!store
            .reserve_local_child_with_runner_capacity(second.id, "lab", 1)
            .unwrap());
        assert_eq!(store.gate_capacity()["active_slots"], 1);
        assert_eq!(store.gate_capacity()["queued_slots"], 1);
        store.fail(gate.id, "test finished").unwrap();
        assert!(store.reconcile_direct_admission(second.id).unwrap());
        std::env::set_var(
            crate::runner_pressure::SAMPLE_ENV,
            r#"{"cpu":9000,"memory":9000}"#,
        );
        assert!(!store.reconcile_direct_admission(second.id).unwrap());
        std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "0");
        std::env::set_var(crate::runner_pressure::SAMPLE_ENV, r#"{"cpu":9000}"#);
        assert!(!store.reconcile_direct_admission(second.id).unwrap());
        std::env::remove_var("HOMEBOY_LAB_GATE_RESERVE");
        std::env::remove_var(crate::runner_pressure::SAMPLE_ENV);
    });
}
