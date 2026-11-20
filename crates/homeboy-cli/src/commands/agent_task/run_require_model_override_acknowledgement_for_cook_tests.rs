use super::*;
use crate::cli_surface::{Cli, Commands};
use clap::Parser;

/// Configure a global rotation policy whose first entry pins
/// `configured-model`, the same shape `configured_rotation_policy()`
/// reads back in [`dispatch_service::require_model_override_acknowledgement`].
fn configure_default_model_route() {
    let mut config = homeboy::core::defaults::load_config();
    config.agent_task.rotation = Some(serde_json::json!({
        "entries": [{ "model": "configured-model" }],
    }));
    homeboy::core::defaults::save_config(&config).expect("save configured model route");
}

fn cook_with_explicit_model(model: &str, acknowledge: bool) -> AgentTaskCookArgs {
    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--prompt".to_string(),
        "fix the deterministic admission fail-fast bug".to_string(),
    ];
    if acknowledge {
        argv.push("--acknowledge-model-override".to_string());
    }
    let cli = Cli::try_parse_from(&argv).expect("parse Cook args");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    *cook
}

/// The submit-time gap #15009 fixes: a Cook whose explicit `--model`
/// displaces a configured route must be refused here, before any
/// unmaterialized Lab admission is persisted — not admitted only to fail
/// this identical, deterministic check on every one of the replay
/// worker's up-to-20 bounded attempts over the following hour.
#[test]
fn unacknowledged_model_override_is_refused_before_any_admission_work() {
    crate::test_support::with_isolated_home(|_| {
        configure_default_model_route();
        let args = cook_with_explicit_model("operator-model", false);
        let error = require_model_override_acknowledgement_for_cook(&args)
            .expect_err("an unacknowledged model override must be refused at submit time");
        assert_eq!(
            error.details["schema"],
            "homeboy/agent-task-model-override-confirmation-required/v1"
        );
        assert_eq!(error.details["selected_model"], "operator-model");
        assert_eq!(
            error.details["configured_primary_model"],
            "configured-model"
        );
        assert_eq!(
            error.details["acknowledgement_flag"],
            "--acknowledge-model-override"
        );
    });
}

#[test]
fn acknowledged_model_override_is_admitted() {
    crate::test_support::with_isolated_home(|_| {
        configure_default_model_route();
        let args = cook_with_explicit_model("operator-model", true);
        require_model_override_acknowledgement_for_cook(&args)
            .expect("an explicit acknowledgement admits the override");
    });
}

#[test]
fn pinning_a_configured_model_requires_acknowledgement() {
    crate::test_support::with_isolated_home(|_| {
        configure_default_model_route();
        let args = cook_with_explicit_model("configured-model", false);
        require_model_override_acknowledgement_for_cook(&args)
            .expect_err("pinning a configured route still suppresses fallback");
    });
}

#[test]
fn no_configured_route_needs_no_acknowledgement() {
    crate::test_support::with_isolated_home(|_| {
        let args = cook_with_explicit_model("operator-model", false);
        require_model_override_acknowledgement_for_cook(&args)
            .expect("an explicit model with no configured route to conflict with is fine");
    });
}
