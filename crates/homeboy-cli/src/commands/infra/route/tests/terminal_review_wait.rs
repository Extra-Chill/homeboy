use super::*;
use clap::Parser;
use homeboy::core::parsed_command_preflight::{
    resolve_parsed_command_preflight, DeferredWorkloadDecision, GenericRoutePolicySnapshot,
    LabReadinessSnapshot, ParsedCommandPolicySnapshot, ParsedCommandPreflightResult,
    ResourceAdmissionEvidence, ResourceHeat,
};

fn review_preflight(wait: bool, ready: bool, path: &Path) -> (Cli, ParsedCommandPreflightResult) {
    let mut argv = vec![
        "homeboy",
        "review",
        "test",
        "--path",
        path.to_str().unwrap(),
        "--skip-lint",
    ];
    if wait {
        argv.push("--wait");
    }
    let cli = Cli::parse_from(&argv);
    let argv = argv.into_iter().map(str::to_string).collect::<Vec<_>>();
    let runner = ready.then(|| "fixture-runner".to_string());
    let input = resource_policy::parsed_command_preflight_input(&cli, &argv);
    let result = resolve_parsed_command_preflight(
        argv,
        input,
        ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::Warm,
            },
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: if ready {
                    "connected_ready"
                } else {
                    "not_configured"
                }
                .to_string(),
                selected_runner_id: runner.clone(),
                available_runner_ids: runner.iter().cloned().collect(),
                reasons: Vec::new(),
                remediation_commands: vec!["homeboy runner status".to_string()],
                repair_admitted_runner_ids: Vec::new(),
            }),
            selected_runner_id: runner.clone(),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: true,
                automatic_authorized: true,
                selected_runner_id: runner,
            },
            deferred_pressure_refusal: !ready,
            runner_admitted: ready,
            runner_incompatible: false,
            auto_local_capacity_fallback: false,
        },
    )
    .expect("resolve immutable admission");
    (cli, result)
}

fn portable_component(home: &Path) -> std::path::PathBuf {
    let component = home.join("component");
    fs::create_dir_all(&component).unwrap();
    fs::write(
        component.join("homeboy.json"),
        r#"{"id":"wait-fixture","extensions":{"portable-db-service":{}}}"#,
    )
    .unwrap();
    let extension = home.join(".config/homeboy/extensions/portable-db-service");
    fs::create_dir_all(&extension).unwrap();
    fs::write(
        extension.join("portable-db-service.json"),
        include_str!(
            "../../../../../../../tests/fixtures/extension_manifests/portable-db-service.json"
        ),
    )
    .unwrap();
    component
}

#[test]
fn unavailable_wait_fails_before_workload_persistence_or_worker_start() {
    crate::test_support::with_isolated_home(|home| {
        // Diagnostic fixture isolation for #15328; not a production root fix.
        let root = home.path().join(".config/homeboy");
        let _env = EnvGuard::set("HOMEBOY_CONFIG_ROOT", root.to_str().unwrap());
        let (cli, preflight) = review_preflight(true, false, home.path());
        assert_eq!(preflight.deferred_workload, DeferredWorkloadDecision::Defer);
        let error = deferred_review_result(&cli, &preflight.normalized_args, &preflight, |_| {
            panic!("terminal observer must not start an asynchronous worker")
        })
        .expect_err("unexecuted tests cannot be successful terminal verification");
        assert!(error.message.contains("no tests executed"));
        assert!(error.message.contains("no deferred workload was enqueued"));
        assert_eq!(error.details["lab_readiness"], "not_configured");
        assert_eq!(error.details["resource_admission"]["kind"], "rejected");
        assert!(homeboy::deferred_workload::records().unwrap().is_empty());
    });
}

#[test]
fn unavailable_nonwait_retains_durable_async_acknowledgment() {
    crate::test_support::with_isolated_home(|home| {
        let root = home.path().join(".config/homeboy");
        let _env = EnvGuard::set("HOMEBOY_CONFIG_ROOT", root.to_str().unwrap());
        let component = portable_component(home.path());
        let (cli, preflight) = review_preflight(false, false, &component);
        let result =
            deferred_review_result(&cli, &preflight.normalized_args, &preflight, |resolved| {
                assert_eq!(resolved, root);
                assert_eq!(homeboy::deferred_workload::records().unwrap().len(), 1);
                Ok(())
            })
            .expect("durable handoff");
        assert_eq!(result["status"], "deferred");
        let records = homeboy::deferred_workload::records().unwrap();
        assert_eq!(result["deferred_workload_id"], records[0].id);
        assert_eq!(
            records[0].state,
            homeboy::deferred_workload::DeferredWorkloadState::Deferred
        );
        assert!(result.get("test_results").is_none());
    });
}

#[test]
fn unavailable_nonwait_worker_failure_is_not_a_success_acknowledgment() {
    crate::test_support::with_isolated_home(|home| {
        let root = home.path().join(".config/homeboy");
        let _env = EnvGuard::set("HOMEBOY_CONFIG_ROOT", root.to_str().unwrap());
        let component = portable_component(home.path());
        let (cli, preflight) = review_preflight(false, false, &component);
        let error = deferred_review_result(&cli, &preflight.normalized_args, &preflight, |_| {
            Err(Error::internal_unexpected("fixture worker failed"))
        })
        .expect_err("failed startup cannot acknowledge ownership");
        assert_eq!(error.message, "fixture worker failed");
        assert_eq!(
            homeboy::deferred_workload::records().unwrap().len(),
            1,
            "retained asynchronous work remains recoverable"
        );
    });
}

#[test]
fn ready_route_uses_existing_dispatch_independently_of_wait_policy() {
    for wait in [false, true] {
        let (cli, preflight) = review_preflight(wait, true, Path::new("."));
        assert_eq!(cli.detach_after_handoff, !wait);
        assert_eq!(
            preflight.deferred_workload,
            DeferredWorkloadDecision::Dispatch
        );
        assert_eq!(
            preflight.generic_route_runner_id.as_deref(),
            Some("fixture-runner")
        );
        reject_deferred_terminal_wait(&cli, &preflight).expect("ready dispatch is unchanged");
    }
}
