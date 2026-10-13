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
            auto_local_capacity_fallback: false,
        },
    )
    .expect("resolve immutable admission");
    (cli, result)
}

#[test]
fn unavailable_wait_fails_before_any_step_is_queued() {
    crate::test_support::with_isolated_home(|home| {
        let (cli, preflight) = review_preflight(true, false, home.path());
        assert_eq!(preflight.deferred_workload, DeferredWorkloadDecision::Defer);
        // The rejection precedes runner resolution and daemon submission, so
        // no daemon is needed for this to fail closed.
        let error = defer_until_runner_eligible(&cli, &preflight.normalized_args, &preflight)
            .expect_err("unexecuted tests cannot be successful terminal verification");
        assert!(error.message.contains("no tests executed"));
        assert!(error.message.contains("nothing was queued"));
        assert_eq!(error.details["lab_readiness"], "not_configured");
        assert_eq!(error.details["resource_admission"]["kind"], "rejected");
    });
}

#[test]
fn runner_wait_step_pins_runner_wait_and_resolved_runner_environment() {
    let overrides = runners::LabJobOverrides {
        env: [("DB_SERVICE_HOST".to_string(), "db.fixture".to_string())].into(),
        secret_env_names: vec!["DB_SERVICE_PASSWORD".to_string()],
        workspace_root: None,
    };
    let portable = portable_route_args(&[
        "homeboy".to_string(),
        "--placement".to_string(),
        "auto".to_string(),
        "review".to_string(),
        "test".to_string(),
        "--skip-lint".to_string(),
    ]);
    let args = runner_wait_step_args(&portable, "lab-a", &overrides);
    assert_eq!(
        args,
        [
            "--runner",
            "lab-a",
            "--wait",
            "--runner-env",
            "DB_SERVICE_HOST=db.fixture",
            "--runner-secret-env",
            "DB_SERVICE_PASSWORD",
            "review",
            "test",
            "--skip-lint",
        ]
    );
    let key = runner_wait_step_key(&args, Path::new("/src"), "lab-a", "content-a").unwrap();
    assert_eq!(
        key,
        runner_wait_step_key(&args, Path::new("/src"), "lab-a", "content-a").unwrap(),
        "an identical deferral converges on one step"
    );
    assert_ne!(
        key,
        runner_wait_step_key(&args, Path::new("/src"), "lab-a", "content-b").unwrap(),
        "changed workspace content is a new step"
    );
    let request = homeboy::core::daemon::step_job::StepJobRequest::new(
        key,
        "/bin/homeboy",
        args,
        "/src",
        homeboy::core::env_materialization_plan::EnvMaterializationPlan::default(),
    )
    .with_wait(homeboy::core::daemon::step_job::StepWait {
        not_before: None,
        runner_eligible: Some("lab-a".to_string()),
    });
    let submission = request.submission().expect("valid runner-wait step");
    assert_eq!(submission["type"], "step");
    assert_eq!(
        submission["request"]["wait"]["runner_eligible"],
        serde_json::json!("lab-a")
    );
}

#[test]
fn ready_route_dispatches_without_deferral_independently_of_wait_policy() {
    for wait in [false, true] {
        let (cli, preflight) = review_preflight(wait, true, Path::new("."));
        assert_eq!(cli.detach_after_handoff, !wait);
        assert_eq!(
            preflight.deferred_workload,
            DeferredWorkloadDecision::NotApplicable
        );
        assert_eq!(
            preflight.generic_route_runner_id.as_deref(),
            Some("fixture-runner")
        );
        reject_deferred_terminal_wait(&cli, &preflight).expect("ready dispatch is unchanged");
    }
}
