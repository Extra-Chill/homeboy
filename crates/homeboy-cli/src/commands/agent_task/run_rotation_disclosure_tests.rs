use super::*;
use clap::Parser;
use homeboy::agents::agent_task_scheduler::{
    AgentTaskExecutionBudget, AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
};

fn plan_with(executions: u32, rotations: u32, entries: usize) -> AgentTaskPlan {
    let mut plan = AgentTaskPlan::new("cook-rotation-disclosure".to_string(), Vec::new());
    plan.options.execution_budget = AgentTaskExecutionBudget::new(executions, 0, rotations);
    plan.options.rotation = (entries > 0).then(|| AgentTaskProviderRotationPolicy {
        entries: (0..entries)
            .map(|index| AgentTaskProviderRotationEntry {
                model: Some(format!("fallback-model-{index}")),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    plan
}

#[test]
fn a_funded_rotation_states_the_providers_and_executions_it_will_use() {
    assert_eq!(
        cook_rotation_disclosure(&plan_with(3, 2, 2)),
        "cook: rotation: 2 fallback provider(s), up to 3 provider execution(s)"
    );
}

#[test]
fn no_rotation_at_all_states_that_plainly() {
    assert_eq!(
        cook_rotation_disclosure(&plan_with(1, 0, 0)),
        "cook: rotation: disabled (1 provider execution(s))"
    );
}

/// A rotation budget that outruns the execution budget can never fire.
#[test]
fn executions_bound_the_rotations_the_disclosure_promises() {
    assert_eq!(
        cook_rotation_disclosure(&plan_with(2, 5, 5)),
        "cook: rotation: 1 fallback provider(s), up to 2 provider execution(s)"
    );
}

#[test]
fn preview_effective_plan_shows_rotation_classes_and_gate_retries() {
    let disclosure = cook_effective_plan_disclosure(&plan_with(3, 2, 2));

    assert!(disclosure.contains("up to 3 executions"), "{disclosure}");
    assert!(
        disclosure.contains("fallback-model-0 → fallback-model-1"),
        "{disclosure}"
    );
    assert!(
        disclosure.contains(
            "provider/transient/timeout/stalled/rate-limited/account-quota-billing-credentials"
        ),
        "{disclosure}"
    );
    assert!(
        disclosure.contains("0 gate retries (same provider; rotations do not fund gates)"),
        "{disclosure}"
    );
}

#[test]
fn confirmed_model_pin_keeps_same_provider_retries() {
    let args = crate::cli_surface::Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--backend",
        "pi",
        "--model",
        "openai/gpt-5.6-terra",
        "--acknowledge-model-override",
        "--max-attempts",
        "2",
        "--no-finalize",
        "--prompt",
        "test",
        "--to-worktree",
        "repo@branch",
    ])
    .expect("parse pinned Cook");
    let crate::cli_surface::Commands::AgentTask(agent_task) = args.command else {
        panic!("agent-task command");
    };
    let crate::commands::agent_task::AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    let mut plan = plan_with(4, 2, 2);
    resolve_cook_execution_budget(&cook, &mut plan).expect("resolve pinned budget");

    assert_eq!(plan.options.execution_budget.max_provider_executions, 2);
    assert_eq!(plan.options.execution_budget.max_same_provider_retries, 1);
    assert_eq!(plan.options.execution_budget.max_provider_rotations, 0);
    assert_eq!(
        plan.metadata["cook_retry_policy"]["route"]["requested"]["model"],
        "openai/gpt-5.6-terra"
    );
    assert_eq!(
        plan.metadata["cook_retry_policy"]["route"]["fallback"]["mode"],
        "pinned"
    );
    assert!(cook_rotation_disclosure(&plan).starts_with("cook: route: pinned;"));
}

#[test]
fn configured_rotation_is_funded_without_override_flags() {
    let args = crate::cli_surface::Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--backend",
        "pi",
        "--max-attempts",
        "2",
        "--no-finalize",
        "--prompt",
        "test",
        "--to-worktree",
        "repo@branch",
    ])
    .expect("parse rotatable Cook");
    let crate::cli_surface::Commands::AgentTask(agent_task) = args.command else {
        panic!("agent-task command");
    };
    let crate::commands::agent_task::AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    let mut plan = plan_with(4, 2, 2);
    resolve_cook_execution_budget(&cook, &mut plan).expect("resolve rotatable budget");

    assert_eq!(plan.options.execution_budget.max_provider_executions, 4);
    assert_eq!(plan.options.execution_budget.max_same_provider_retries, 1);
    assert_eq!(plan.options.execution_budget.max_provider_rotations, 2);
    assert_eq!(
        plan.metadata["cook_retry_policy"]["route"]["fallback"]["mode"],
        "rotatable"
    );
}
