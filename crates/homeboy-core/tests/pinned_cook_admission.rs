use homeboy_core::parsed_command_preflight::*;
use homeboy_lab_contract::{
    EffectiveExecutionPlacement, ExecutionPlacementRequirement, RunnerSelectionSource,
};

#[test]
fn pinned_cook_waits_for_capacity_without_losing_its_runner() {
    let input = ParsedCommandPreflightInput {
        identity: ParsedCommandIdentity {
            family: "agent-task".into(),
            operation: vec!["cook".into()],
        },
        resource_admission: ResourceAdmissionRequirement::Exempt,
        controller_execution: ControllerExecution::SplitPlacementCoordinator,
        deferred_workload: DeferredWorkloadPolicy::Forbidden,
        placement: PlacementIntent::Auto,
        runner: RunnerIntent::Explicit("lab-b".into()),
        runner_normalization: RunnerNormalization::PinnedCookArgv,
        lab_route: LabRouteIntent::Supported { automatic: true },
        provenance: ProvenanceRequirement::CaptureExecution,
    };
    for (state, reason, waits) in [
        ("capacity_blocked", "pressure", true),
        ("capacity_blocked", "capacity_reached", true),
        ("connected_ready", "", false),
        ("disconnected", "runner_disconnected", false),
        ("stale", "daemon_drift", false),
        ("absent", "unknown_runner", false),
    ] {
        let ready = state == "connected_ready";
        let policy = ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: state.into(),
                selected_runner_id: ready.then(|| "lab-b".into()),
                available_runner_ids: if ready { vec!["lab-b".into()] } else { vec![] },
                reasons: vec![reason.into()],
                remediation_commands: vec![],
                repair_admitted_runner_ids: vec![],
            }),
            selected_runner_id: Some("lab-b".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: false,
                automatic_authorized: false,
                selected_runner_id: Some("lab-b".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted: ready,
            auto_local_capacity_fallback: false,
        };
        let result = resolve_parsed_command_preflight(
            vec![
                "homeboy".into(),
                "--runner".into(),
                "lab-b".into(),
                "agent-task".into(),
                "cook".into(),
            ],
            input.clone(),
            policy,
        );
        if !waits && !ready {
            assert!(result.is_err(), "{state} must refuse");
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{state}/{reason}: {error}"));
        assert_eq!(result.queued_for_lab_admission(), waits, "{state}");
        assert_eq!(
            result.placement.required,
            ExecutionPlacementRequirement::Lab
        );
        assert_eq!(result.placement.selected, EffectiveExecutionPlacement::Lab);
        let runner = result.placement.runner.unwrap();
        assert_eq!(runner.runner_id, "lab-b");
        assert_eq!(runner.source, RunnerSelectionSource::Explicit);
        assert!(!result.placement.fallback.local_allowed);
    }
}
