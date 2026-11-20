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

#[test]
fn a_provider_failure_asks_which_provider_was_registered_before_retrying() {
    let actions = actions_for(AgentTaskFailureClassification::Provider, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task providers",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
    assert_eq!(
        repair_commands(&actions),
        vec!["homeboy --placement local agent-task retry run-1 --run"]
    );
}

#[test]
fn a_transient_failure_leads_with_the_retry_it_is_documented_to_survive() {
    let actions = actions_for(AgentTaskFailureClassification::Transient, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy --placement local agent-task retry run-1 --run",
            "homeboy runs evidence run-1 --task task-a --failure-only",
        ]
    );
}

#[test]
fn a_timeout_reviews_the_candidate_before_spending_another_attempt() {
    let actions = actions_for(AgentTaskFailureClassification::Timeout, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task review run-1",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
}

#[test]
fn a_stalled_run_reconciles_and_repairs_the_runner_that_went_quiet() {
    let actions = actions_for(AgentTaskFailureClassification::Stalled, Some("homeboy-lab"));

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy agent-task reconcile run-1 --dry-run",
            "homeboy runner status homeboy-lab",
            "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
    assert_eq!(
        repair_commands(&actions),
        vec![
            "homeboy agent-task reconcile run-1 --dry-run",
            "homeboy runner doctor homeboy-lab --scope lab-offload --repair",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
}

#[test]
fn a_stalled_run_without_a_known_runner_never_names_one() {
    let actions = actions_for(AgentTaskFailureClassification::Stalled, None);

    assert!(!commands(&actions)
        .iter()
        .any(|command| command.starts_with("homeboy runner ")));
}

#[test]
fn a_rate_limited_failure_offers_provider_rotation() {
    let actions = actions_for(AgentTaskFailureClassification::RateLimited, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task providers",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
}

#[test]
fn a_policy_denial_never_suggests_replaying_the_denied_request() {
    let actions = actions_for(AgentTaskFailureClassification::PolicyDenied, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task status run-1",
        ]
    );
    assert!(repair_commands(&actions).is_empty());
}

#[test]
fn a_missing_capability_runs_the_readiness_repair_chain_not_another_attempt() {
    let actions = actions_for(
        AgentTaskFailureClassification::CapabilityMissing,
        Some("homeboy-lab"),
    );

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task providers --full",
            "homeboy agent-task doctor --runner homeboy-lab",
            "homeboy agent-task doctor --runner homeboy-lab --repair",
        ]
    );
    assert_eq!(
        repair_commands(&actions),
        vec!["homeboy agent-task doctor --runner homeboy-lab --repair"]
    );
}

#[test]
fn invalid_input_shows_failure_evidence_and_never_offers_retry() {
    let retry = owner_bound_retry_action("run-1", None, json!({ "placement": "local" }));
    let failure = DiagnosedFailure {
        task_id: "task-a".to_string(),
        classification: AgentTaskFailureClassification::InvalidInput,
        phase: None,
        provider_budget_consumed: false,
    };

    let (actions, basis) =
        diagnose_next_actions("run-1", &[failure], &[], None, Some(&retry), false, false);

    assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
    assert_eq!(
        commands(&actions),
        vec!["homeboy runs evidence run-1 --task task-a --failure-only"]
    );
}

#[test]
fn an_execution_failure_shows_the_failing_step_and_what_it_produced() {
    let actions = actions_for(AgentTaskFailureClassification::ExecutionFailed, None);

    assert_eq!(
        commands(&actions),
        vec![
            "homeboy runs evidence run-1 --task task-a --failure-only",
            "homeboy agent-task review run-1",
            "homeboy runs artifacts run-1 --full",
            "homeboy --placement local agent-task retry run-1 --run",
        ]
    );
}

#[test]
fn an_unclassifiable_failure_falls_back_to_the_generic_set() {
    let (actions, basis) = diagnose_next_actions(
        "run-1",
        &[failure(AgentTaskFailureClassification::Unknown)],
        &[],
        None,
        None,
        false,
        false,
    );

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

#[test]
fn a_run_with_no_diagnosis_at_all_falls_back_to_the_generic_set() {
    let (actions, basis) = diagnose_next_actions("run-1", &[], &[], None, None, false, false);

    assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
    assert_eq!(actions.len(), 3);
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
