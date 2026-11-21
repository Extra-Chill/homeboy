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
        // #17832: probe timeouts and freshness gaps during a runner upgrade
        // are momentary; a pinned Cook waits through them too.
        ("connected_ineligible", "daemon_freshness_unavailable", true),
        ("readiness_unobserved", "status_probe_timeout", true),
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

/// #17832: unpinned `--placement lab` queues on the same transient states
/// instead of refusing with `required Lab placement has no selected ready
/// runner`, and still refuses on hard substrate failures.
#[test]
fn lab_placement_cook_waits_for_transient_readiness() {
    let input = ParsedCommandPreflightInput {
        identity: ParsedCommandIdentity {
            family: "agent-task".into(),
            operation: vec!["cook".into()],
        },
        resource_admission: ResourceAdmissionRequirement::Exempt,
        controller_execution: ControllerExecution::SplitPlacementCoordinator,
        deferred_workload: DeferredWorkloadPolicy::Forbidden,
        placement: PlacementIntent::Lab,
        runner: RunnerIntent::Default,
        runner_normalization: RunnerNormalization::None,
        lab_route: LabRouteIntent::Supported { automatic: true },
        provenance: ProvenanceRequirement::CaptureExecution,
    };
    for (state, reason, waits) in [
        ("capacity_blocked", "pressure", true),
        ("connected_ineligible", "daemon_unverified", true),
        ("readiness_unobserved", "status_probe_timeout", true),
        ("disconnected", "runner_disconnected", false),
        ("stale", "daemon_drift", false),
    ] {
        let policy = ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: state.into(),
                selected_runner_id: Some("homeboy-lab".into()),
                available_runner_ids: vec![],
                reasons: vec![reason.into()],
                remediation_commands: vec![],
                repair_admitted_runner_ids: vec![],
            }),
            selected_runner_id: Some("homeboy-lab".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: false,
                automatic_authorized: false,
                selected_runner_id: Some("homeboy-lab".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted: false,
            auto_local_capacity_fallback: false,
        };
        let result = resolve_parsed_command_preflight(
            vec!["homeboy".into(), "agent-task".into(), "cook".into()],
            input.clone(),
            policy,
        );
        if !waits {
            assert!(result.is_err(), "{state} must refuse");
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{state}/{reason}: {error}"));
        assert!(result.queued_for_lab_admission(), "{state}");
        assert_eq!(result.placement.selected, EffectiveExecutionPlacement::Lab);
        assert_eq!(result.selected_runner_id.as_deref(), Some("homeboy-lab"));
        assert!(!result.placement.fallback.local_allowed);
    }
}
