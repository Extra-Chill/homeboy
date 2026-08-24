//! Durable command provenance shared by observation-producing commands.

use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

use crate::cli_surface::Placement;

const SCHEMA: &str = "homeboy/execution-provenance/v1";

fn captured_storage() -> &'static RwLock<Option<Value>> {
    static STORAGE: OnceLock<RwLock<Option<Value>>> = OnceLock::new();
    STORAGE.get_or_init(|| RwLock::new(None))
}

/// Capture normalized, redacted intent before placement routing consumes its
/// transport markers. The same original argv reaches a Lab child, where its
/// resolved execution identity is recorded independently.
pub fn capture(result: &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult) {
    let mut slot = captured_storage()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        *slot = Some(build(result));
    }
}

/// Capture descriptor-composed command intent without requiring a synthetic
/// static `Commands` value.
pub fn capture_composed(
    placement: Placement,
    runner: Option<&str>,
    detach_after_handoff: bool,
    allow_dirty_lab_workspace: bool,
    skip_deps_hydration: bool,
    runner_env: &[String],
    lab_env_json: Option<&str>,
    normalized_args: &[String],
) {
    let mut slot = captured_storage()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        let execution = homeboy::core::resource_policy_context::lab_execution_runner_id()
            .map(|runner_id| ("lab", Some(runner_id)))
            .unwrap_or(("controller", None));
        let argv = redact_execution_argv(normalized_args);
        *slot = Some(json!({
            "schema": SCHEMA,
            "operator_intent": {
                "argv": argv,
                "rerun_command": homeboy::core::redaction::redact_argv_shell_display(&redact_execution_argv(normalized_args)),
                "placement": placement_name(placement),
                "runner_id": runner,
                "global_flags": {
                    "detach_after_handoff": detach_after_handoff,
                    "allow_dirty_lab_workspace": allow_dirty_lab_workspace,
                    "skip_deps_hydration": skip_deps_hydration,
                    "runner_env": runner_env.iter().map(|value| redact_env_assignment(value, &homeboy::core::redaction::RedactionPolicy::default())).collect::<Vec<_>>(),
                    "lab_env_json": lab_env_json.map(|value| redact_json_env(value, &homeboy::core::redaction::RedactionPolicy::default())),
                },
            },
            "resolved_execution": { "location": execution.0, "runner_id": execution.1 },
            "resource_policy": {
                "decision_origin": decision_origin(placement, runner.is_some(), execution.0),
                "preflight": crate::commands::utils::resource_policy::captured_context().as_ref().map(crate::commands::utils::resource_policy::resource_policy_context_to_json),
            },
        }));
    }
}

/// Return the command provenance captured for this process, if any. Older
/// callers and imported observations remain valid without this optional field.
pub fn captured() -> Option<Value> {
    captured_storage().read().ok().and_then(|slot| slot.clone())
}

fn build(result: &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult) -> Value {
    let argv = redact_execution_argv(&result.normalized_args);
    let rerun_command = homeboy::core::redaction::redact_argv_shell_display(&argv);
    let placement = match result.placement.requested {
        homeboy_lab_runner_contract::Placement::Auto => "auto",
        homeboy_lab_runner_contract::Placement::Local => "local",
        homeboy_lab_runner_contract::Placement::Lab => "lab",
        homeboy_lab_runner_contract::Placement::LabOrLocal => "lab-or-local",
    };
    let directive_location = match result.placement.selected {
        homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab => "lab",
        homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local => "controller",
    };
    // Split-placement coordinators dispatch children later; this invocation is
    // still controller-owned even when its child placement directive is Lab.
    let location = if result.input.controller_execution
        == homeboy::core::parsed_command_preflight::ControllerExecution::Ordinary
    {
        directive_location
    } else {
        "controller"
    };
    let policy_runner_id = result
        .placement
        .runner
        .as_ref()
        .filter(|runner| {
            runner.source == homeboy_lab_runner_contract::RunnerSelectionSource::Policy
        })
        .map(|runner| runner.runner_id.clone());
    let explicit_runner_id = match &result.input.runner {
        homeboy::core::parsed_command_preflight::RunnerIntent::Explicit(runner_id) => {
            Some(runner_id.clone())
        }
        _ => None,
    };
    let runner_id = (location == "lab")
        .then(|| {
            explicit_runner_id
                .clone()
                .or_else(|| policy_runner_id.clone())
        })
        .flatten();
    let decision_origin =
        if result.placement.requested == homeboy_lab_runner_contract::Placement::Lab {
            "explicit"
        } else {
            match result.placement.runner.as_ref().map(|runner| runner.source) {
                Some(homeboy_lab_runner_contract::RunnerSelectionSource::Explicit) => "explicit",
                Some(homeboy_lab_runner_contract::RunnerSelectionSource::Policy) => "automatic",
                None if result.placement.override_authorization.authorized => "explicit",
                None if result.placement.fallback.local_allowed => "fallback",
                _ => "automatic",
            }
        };
    let runner_env = flag_values(&argv, "--runner-env");
    let lab_env_json = flag_values(&argv, "--lab-env-json").into_iter().last();

    json!({
        "schema": SCHEMA,
        "operator_intent": {
            "argv": argv,
            "rerun_command": rerun_command,
            "placement": placement,
            "runner_id": explicit_runner_id,
            "global_flags": {
                "detach_after_handoff": argv.iter().any(|arg| arg == "--detach-after-handoff"),
                "allow_dirty_lab_workspace": argv.iter().any(|arg| arg == "--allow-dirty-lab-workspace"),
                "skip_deps_hydration": argv.iter().any(|arg| arg == "--skip-deps-hydration"),
                "runner_env": runner_env,
                "lab_env_json": lab_env_json,
            },
        },
        "resolved_execution": {
            "location": location,
            "runner_id": runner_id,
        },
        "resource_policy": {
            "decision_origin": decision_origin,
            "preflight": result.resource_policy.as_ref()
                .map(crate::commands::utils::resource_policy::resource_policy_context_to_json),
        },
    })
}

fn flag_values(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut next = false;
    for arg in args {
        if next {
            values.push(arg.clone());
            next = false;
        } else if arg == flag {
            next = true;
        } else if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            values.push(value.to_string());
        }
    }
    values
}

fn redact_execution_argv(args: &[String]) -> Vec<String> {
    let policy = homeboy::core::redaction::RedactionPolicy::default();
    let args = args
        .iter()
        .filter(|arg| arg.as_str() != crate::commands::utils::args::EXPLICIT_PASSTHROUGH_SENTINEL)
        .cloned()
        .collect::<Vec<_>>();
    let mut redacted = homeboy::core::redaction::redact_argv(&args);
    for index in 0..args.len() {
        let replacement = match args[index].as_str() {
            "--runner-env" if index + 1 < args.len() => {
                Some(redact_env_assignment(&args[index + 1], &policy))
            }
            "--lab-env-json" if index + 1 < args.len() => {
                Some(redact_json_env(&args[index + 1], &policy))
            }
            arg if arg.starts_with("--runner-env=") => Some(format!(
                "--runner-env={}",
                redact_env_assignment(&arg[13..], &policy)
            )),
            arg if arg.starts_with("--lab-env-json=") => Some(format!(
                "--lab-env-json={}",
                redact_json_env(&arg[15..], &policy)
            )),
            _ => None,
        };
        if let Some(replacement) = replacement {
            if matches!(args[index].as_str(), "--runner-env" | "--lab-env-json") {
                redacted[index + 1] = replacement;
            } else {
                redacted[index] = replacement;
            }
        }
    }
    redacted
}

fn redact_env_assignment(
    value: &str,
    policy: &homeboy::core::redaction::RedactionPolicy,
) -> String {
    let Some((key, secret)) = value.split_once('=') else {
        return policy.redact_env_value(value);
    };
    if is_sensitive_env_key(key) {
        return format!("{key}=[redacted]");
    }
    policy
        .redact_json(&json!({ key: secret }))
        .get(key)
        .and_then(Value::as_str)
        .map(|secret| format!("{key}={secret}"))
        .unwrap_or_else(|| policy.redact_env_value(value))
}

fn is_sensitive_env_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "credential",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn redact_json_env(value: &str, policy: &homeboy::core::redaction::RedactionPolicy) -> String {
    serde_json::from_str::<Value>(value)
        .map(|value| policy.redact_json(&value).to_string())
        .unwrap_or_else(|_| policy.redact_env_value(value))
}

fn placement_name(placement: Placement) -> &'static str {
    match placement {
        Placement::Auto => "auto",
        Placement::Local => "local",
        Placement::Lab => "lab",
        Placement::LabOrLocal => "lab-or-local",
    }
}

fn decision_origin(placement: Placement, has_runner: bool, location: &str) -> &'static str {
    match (has_runner, placement, location) {
        (true, _, _) | (_, Placement::Local | Placement::Lab, _) => "explicit",
        (_, Placement::LabOrLocal, "controller") => "fallback",
        _ => "automatic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::Cli;
    use clap::Parser;

    fn provenance(args: &[&str]) -> Value {
        let argv = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let cli = Cli::try_parse_from(&argv).expect("parse CLI");
        let runner_id = cli.runner.as_deref();
        build(
            &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
                argv.clone(),
                crate::commands::utils::resource_policy::parsed_command_preflight_input(
                    &cli, &argv,
                ),
                None,
                None,
                homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
                homeboy::core::parsed_command_preflight::FallbackDirective::None,
                crate::cli_runtime::placement_directive(&cli, runner_id, false),
                runner_id.map(str::to_string),
            ),
        )
    }

    #[test]
    fn preserves_explicit_local_lab_and_runner_intent() {
        for (args, placement, runner, rerun_fragment) in [
            (
                vec!["homeboy", "--placement", "local", "review"],
                "local",
                None,
                "--placement local",
            ),
            (
                vec!["homeboy", "--placement", "lab", "review"],
                "lab",
                None,
                "--placement lab",
            ),
            (
                vec!["homeboy", "--runner", "lab-a", "review"],
                "auto",
                Some("lab-a"),
                "--runner lab-a",
            ),
        ] {
            let value = provenance(&args);
            assert_eq!(value["operator_intent"]["placement"], placement);
            assert_eq!(value["operator_intent"]["runner_id"], json!(runner));
            assert_eq!(value["resource_policy"]["decision_origin"], "explicit");
            assert!(value["operator_intent"]["rerun_command"]
                .as_str()
                .expect("rerun command")
                .contains(rerun_fragment));
        }
    }

    #[test]
    fn records_lab_or_local_controller_execution_as_fallback() {
        let value = provenance(&["homeboy", "--placement=lab-or-local", "review"]);

        assert_eq!(value["resolved_execution"]["location"], "controller");
        assert_eq!(value["resource_policy"]["decision_origin"], "fallback");
        assert!(value["operator_intent"]["rerun_command"]
            .as_str()
            .expect("rerun command")
            .contains("--placement=lab-or-local"));
    }

    #[test]
    fn composed_decision_origin_matches_builtin_semantics() {
        assert_eq!(
            decision_origin(Placement::LabOrLocal, false, "controller"),
            "fallback"
        );
        assert_eq!(
            decision_origin(Placement::Lab, false, "controller"),
            "explicit"
        );
        assert_eq!(
            decision_origin(Placement::Auto, true, "controller"),
            "explicit"
        );
        assert_eq!(
            decision_origin(Placement::Auto, false, "controller"),
            "automatic"
        );
    }

    #[test]
    fn records_resolved_lab_runner_separately_from_lab_intent() {
        let args = ["homeboy", "--placement", "lab", "review"];
        let argv = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let cli = Cli::try_parse_from(&argv).expect("parse CLI");
        let value = build(
            &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
                argv.clone(),
                crate::commands::utils::resource_policy::parsed_command_preflight_input(
                    &cli, &argv,
                ),
                None,
                None,
                homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
                homeboy::core::parsed_command_preflight::FallbackDirective::None,
                crate::cli_runtime::placement_directive(&cli, Some("runner-a"), false),
                Some("runner-a".to_string()),
            ),
        );

        assert_eq!(value["operator_intent"]["placement"], "lab");
        assert_eq!(value["operator_intent"]["runner_id"], Value::Null);
        assert_eq!(value["resolved_execution"]["location"], "lab");
        assert_eq!(value["resolved_execution"]["runner_id"], "runner-a");
        assert!(value["resource_policy"].get("policy_runner_id").is_none());
        assert_eq!(value["resource_policy"]["decision_origin"], "explicit");
    }

    #[test]
    fn v1_resource_policy_shape_has_no_policy_runner_field() {
        let value = provenance(&["homeboy", "review"]);
        let policy = value["resource_policy"].as_object().expect("policy object");
        assert_eq!(
            policy.keys().map(String::as_str).collect::<Vec<_>>(),
            ["decision_origin", "preflight"]
        );
        assert!(!serde_json::to_string(&value)
            .expect("serialize provenance")
            .contains("policy_runner_id"));
    }

    #[test]
    fn redacts_secrets_without_dropping_placement() {
        let value = provenance(&[
            "homeboy",
            "--placement",
            "local",
            "--runner-env",
            "API_TOKEN=secret-value",
            "review",
        ]);
        let command = value["operator_intent"]["rerun_command"]
            .as_str()
            .expect("rerun command");

        assert!(command.contains("--placement local"));
        assert!(!command.contains("secret-value"));
    }

    #[test]
    fn preserves_redacted_runner_and_lab_environment_flag_shapes() {
        let value = provenance(&[
            "homeboy",
            "--runner-env",
            "MODE=test",
            "--runner-env=API_TOKEN=secret-value",
            "--lab-env-json",
            "{\"token\":\"secret-value\",\"mode\":\"test\"}",
            "review",
        ]);
        assert_eq!(
            value["operator_intent"]["global_flags"]["runner_env"],
            json!(["MODE=test", "API_TOKEN=[redacted]"])
        );
        assert_eq!(
            value["operator_intent"]["global_flags"]["lab_env_json"],
            json!("{\"mode\":\"test\",\"token\":\"[REDACTED]\"}")
        );
    }

    #[test]
    fn rerun_command_omits_internal_passthrough_marker() {
        let value = provenance(&[
            "homeboy",
            "review",
            "test",
            "--",
            crate::commands::utils::args::EXPLICIT_PASSTHROUGH_SENTINEL,
            "--filter=SmokeTest",
        ]);
        let command = value["operator_intent"]["rerun_command"]
            .as_str()
            .expect("rerun command");

        assert!(command.contains("-- --filter=SmokeTest"));
        assert!(!command.contains(crate::commands::utils::args::EXPLICIT_PASSTHROUGH_SENTINEL));
    }

    #[test]
    fn bench_provenance_preserves_passthrough_and_caller_run_id_with_redaction() {
        let value = provenance(&[
            "homeboy",
            "bench",
            "--rig=static-site-importer-fixture-matrix",
            "--run-id",
            "fixture87-forms-frontend-proof-retry",
            "--",
            "--fixture-root",
            "/tmp/fixture root",
            "--fixture-id=87",
            "--api-token=secret-value",
        ]);
        let argv = value["operator_intent"]["argv"]
            .as_array()
            .expect("canonical argv");
        let command = value["operator_intent"]["rerun_command"]
            .as_str()
            .expect("shell-safe rerun command");

        assert_eq!(argv[4], "fixture87-forms-frontend-proof-retry");
        assert_eq!(argv[6], "--fixture-root");
        assert_eq!(argv[7], "/tmp/fixture root");
        assert_eq!(argv[8], "--fixture-id=87");
        assert_eq!(argv[9], "--api-token=[REDACTED]");
        assert!(command.contains("--run-id fixture87-forms-frontend-proof-retry"));
        assert!(command.contains("--fixture-root '/tmp/fixture root'"));
        assert!(!command.contains("secret-value"));
    }
}
