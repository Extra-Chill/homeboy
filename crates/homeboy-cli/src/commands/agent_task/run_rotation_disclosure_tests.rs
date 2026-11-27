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

/// The disclosure states the fallbacks and executions the plan can really
/// use: a rotation budget that outruns the execution budget can never fire.
#[test]
fn rotation_disclosure_states_the_reachable_fallbacks_and_executions() {
    for ((executions, rotations, entries), expected) in [
        (
            (3, 2, 2),
            "cook: rotation: 2 fallback provider(s), up to 3 provider execution(s)",
        ),
        (
            (1, 0, 0),
            "cook: rotation: disabled (1 provider execution(s))",
        ),
        (
            (2, 5, 5),
            "cook: rotation: 1 fallback provider(s), up to 2 provider execution(s)",
        ),
    ] {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(executions, rotations, entries)),
            expected
        );
    }
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

fn resolved_budget_plan(extra: &[&str]) -> AgentTaskPlan {
    let mut argv = vec!["homeboy", "agent-task", "cook", "--backend", "pi"];
    argv.extend_from_slice(extra);
    argv.extend([
        "--max-attempts",
        "2",
        "--no-finalize",
        "--prompt",
        "test",
        "--to-worktree",
        "repo@branch",
    ]);
    let args = crate::cli_surface::Cli::try_parse_from(argv).expect("parse Cook");
    let crate::cli_surface::Commands::AgentTask(agent_task) = args.command else {
        panic!("agent-task command");
    };
    let crate::commands::agent_task::AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    let mut plan = plan_with(4, 2, 2);
    resolve_cook_execution_budget(&cook, &mut plan).expect("resolve budget");
    plan
}

/// A confirmed model pin keeps same-provider retries but funds no rotation;
/// a configured rotation is funded without any override flags.
#[test]
fn execution_budget_funds_rotation_only_for_an_unpinned_route() {
    let pinned = resolved_budget_plan(&[
        "--model",
        "openai/gpt-5.6-terra",
        "--acknowledge-model-override",
    ]);
    let budget = &pinned.options.execution_budget;
    assert_eq!(
        (
            budget.max_provider_executions,
            budget.max_same_provider_retries,
            budget.max_provider_rotations
        ),
        (2, 1, 0)
    );
    assert_eq!(
        pinned.metadata["cook_retry_policy"]["route"]["requested"]["model"],
        "openai/gpt-5.6-terra"
    );
    assert_eq!(
        pinned.metadata["cook_retry_policy"]["route"]["fallback"]["mode"],
        "pinned"
    );
    assert!(cook_rotation_disclosure(&pinned).starts_with("cook: route: pinned;"));

    let rotatable = resolved_budget_plan(&[]);
    let budget = &rotatable.options.execution_budget;
    assert_eq!(
        (
            budget.max_provider_executions,
            budget.max_same_provider_retries,
            budget.max_provider_rotations
        ),
        (4, 1, 2)
    );
    assert_eq!(
        rotatable.metadata["cook_retry_policy"]["route"]["fallback"]["mode"],
        "rotatable"
    );
}
