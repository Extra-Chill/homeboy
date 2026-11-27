use super::*;

fn failure(classification: AgentTaskFailureClassification) -> DiagnosedFailure {
    DiagnosedFailure {
        task_id: "task-a".to_string(),
        classification,
        phase: None,
        provider_budget_consumed: false,
    }
}

fn commands(actions: &[CommandNextAction]) -> Vec<&str> {
    actions
        .iter()
        .map(|action| action.command.as_str())
        .collect()
}

fn repair_commands(actions: &[CommandNextAction]) -> Vec<&str> {
    actions
        .iter()
        .filter(|action| matches!(action.kind, Some(CommandNextActionKind::Repair)))
        .map(|action| action.command.as_str())
        .collect()
}

fn actions_for(
    classification: AgentTaskFailureClassification,
    runner_id: Option<&str>,
) -> Vec<CommandNextAction> {
    let retry = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));
    let (actions, basis) = diagnose_next_actions(
        "run-1",
        &[failure(classification)],
        &[],
        runner_id,
        Some(&retry),
        false,
        false,
    );
    assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
    actions
}

/// Each classification maps to its own ordered recovery chain; the repair
/// subset is what an operator may run without first reading evidence.
#[test]
fn each_failure_classification_yields_its_ordered_recovery_chain() {
    use AgentTaskFailureClassification::*;
    const EVIDENCE: &str = "homeboy runs evidence run-1 --task task-a --failure-only";
    const RETRY: &str = "homeboy --placement local agent-task retry run-1 --run";
    for (classification, runner, expected, repair) in [
        (
            Provider,
            None,
            vec![EVIDENCE, "homeboy agent-task providers", RETRY],
            Some(vec![RETRY]),
        ),
        (Transient, None, vec![RETRY, EVIDENCE], None),
        (
            Timeout,
            None,
            vec![EVIDENCE, "homeboy agent-task review run-1", RETRY],
            None,
        ),
        (
            Stalled,
            Some("homeboy-lab"),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner status homeboy-lab",
                "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
                EVIDENCE,
                RETRY,
            ],
            Some(vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
                RETRY,
            ]),
        ),
        (
            RateLimited,
            None,
            vec![EVIDENCE, "homeboy agent-task providers", RETRY],
            None,
        ),
        // A policy denial never suggests replaying the denied request.
        (
            PolicyDenied,
            None,
            vec![EVIDENCE, "homeboy agent-task status run-1"],
            Some(vec![]),
        ),
        // A missing capability runs the readiness repair chain, not another attempt.
        (
            CapabilityMissing,
            Some("homeboy-lab"),
            vec![
                EVIDENCE,
                "homeboy agent-task providers --full",
                "homeboy agent-task doctor --runner homeboy-lab",
                "homeboy agent-task doctor --runner homeboy-lab --repair",
            ],
            Some(vec![
                "homeboy agent-task doctor --runner homeboy-lab --repair",
            ]),
        ),
        // Invalid input never offers a retry.
        (InvalidInput, None, vec![EVIDENCE], None),
        (
            ExecutionFailed,
            None,
            vec![
                EVIDENCE,
                "homeboy agent-task review run-1",
                "homeboy runs artifacts run-1 --full",
                RETRY,
            ],
            None,
        ),
    ] {
        let label = format!("{classification:?}");
        let actions = actions_for(classification, runner);
        assert_eq!(commands(&actions), expected, "{label}");
        if let Some(repair) = repair {
            assert_eq!(repair_commands(&actions), repair, "{label}");
        }
    }
}

#[test]
fn a_stalled_run_without_a_known_runner_never_names_one() {
    let actions = actions_for(AgentTaskFailureClassification::Stalled, None);

    assert!(!commands(&actions)
        .iter()
        .any(|command| command.starts_with("homeboy runner ")));
}

#[test]
fn unclassifiable_or_undiagnosed_failures_fall_back_to_the_generic_set() {
    for failures in [
        vec![failure(AgentTaskFailureClassification::Unknown)],
        Vec::new(),
    ] {
        let (actions, basis) =
            diagnose_next_actions("run-1", &failures, &[], None, None, false, false);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task status run-1",
                "homeboy runs artifacts run-1",
                "homeboy agent-task review run-1",
            ]
        );
    }
}

#[test]
fn missing_artifacts_name_the_task_and_the_artifacts_that_were_not_produced() {
    let missing = vec![json!({
        "task_id": "task-b",
        "missing": ["concept_packet", "design_packet"],
    })];

    let (actions, basis) = diagnose_next_actions("run-1", &[], &missing, None, None, false, false);

    assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs artifacts run-1 --full",
            "homeboy runs evidence run-1 --task task-b --failure-only",
        ]
    );
    assert!(actions[1].label.contains("concept_packet, design_packet"));
}

#[test]
fn classification_and_missing_artifact_actions_are_merged_without_duplicates() {
    let missing = vec![json!({ "task_id": "task-a", "missing": ["patch"] })];

    let (actions, _) = diagnose_next_actions(
        "run-1",
        &[failure(AgentTaskFailureClassification::ExecutionFailed)],
        &missing,
        None,
        None,
        false,
        false,
    );

    let commands = commands(&actions);
    assert_eq!(
        commands
            .iter()
            .filter(
                |command| **command == "homeboy runs evidence run-1 --task task-a --failure-only"
            )
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| **command == "homeboy runs artifacts run-1 --full")
            .count(),
        1
    );
}

#[test]
fn run_and_task_ids_are_shell_quoted_in_every_emitted_command() {
    let (actions, _) = diagnose_next_actions(
        "run with spaces",
        &[DiagnosedFailure {
            task_id: "task with spaces".to_string(),
            classification: AgentTaskFailureClassification::InvalidInput,
            phase: None,
            provider_budget_consumed: false,
        }],
        &[],
        None,
        None,
        false,
        false,
    );

    assert_eq!(
        commands(&actions),
        vec!["homeboy runs evidence 'run with spaces' --task 'task with spaces' --failure-only",]
    );
}

#[test]
fn runner_cancellation_uses_runner_evidence_before_a_proven_replay() {
    let retry = owner_bound_retry_action("run-1", Some("homeboy-lab"), json!({}));
    let (actions, basis) = diagnose_next_actions(
        "run-1",
        &[],
        &[],
        Some("homeboy-lab"),
        Some(&retry),
        true,
        false,
    );

    assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
    assert_eq!(
        commands(&actions),
        vec![
            "homeboy agent-task status run-1",
            "homeboy runner status homeboy-lab",
            "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
            "homeboy --runner homeboy-lab agent-task retry run-1 --run",
        ]
    );
}

fn lab_required_decision() -> homeboy_lab_contract::ExecutionPlacementDecision {
    use homeboy_lab_contract::*;
    ExecutionPlacementDecision::new(
        "cook-placement",
        "v1",
        ExecutionPlacementIdentity {
            repository: "extrachill".to_string(),
            workspace: "/tmp/extrachill".to_string(),
            task: "cook".to_string(),
            candidate: None,
            base: None,
        },
        Placement::Lab,
        ExecutionPlacementRequirement::Lab,
        EffectiveExecutionPlacement::Lab,
        Some(ExecutionPlacementRunnerSelection {
            runner_id: "homeboy-lab".to_string(),
            source: RunnerSelectionSource::Explicit,
        }),
        ExecutionPlacementFallback {
            local_allowed: false,
            reason: None,
        },
        ExecutionPlacementOverrideAuthorization {
            authorized: false,
            authority: None,
        },
    )
}

/// #19181: a Lab-required mission's retry must never be advertised as a
/// controller-local replay.
#[test]
fn lab_required_retry_action_never_emits_placement_local() {
    assert!(lab_required(&lab_required_decision()));
    let retry = owner_bound_retry_action("run-1", None, json!({ "placement": "lab" }));
    assert_eq!(
        retry.command,
        "homeboy --placement lab agent-task retry run-1 --run"
    );
    assert!(!retry.command.contains("--placement local"));

    let retry = owner_bound_retry_action(
        "run-1",
        lab_required_decision()
            .runner
            .as_ref()
            .map(|runner| runner.runner_id.as_str()),
        json!({ "placement": "runner", "runner_id": "homeboy-lab" }),
    );
    assert_eq!(
        retry.command,
        "homeboy --runner homeboy-lab agent-task retry run-1 --run"
    );
}
