//! Durable command provenance shared by observation-producing commands.

use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

use crate::cli_surface::{Cli, Placement};

const SCHEMA: &str = "homeboy/execution-provenance/v1";

fn captured_storage() -> &'static RwLock<Option<Value>> {
    static STORAGE: OnceLock<RwLock<Option<Value>>> = OnceLock::new();
    STORAGE.get_or_init(|| RwLock::new(None))
}

/// Capture normalized, redacted intent before placement routing consumes its
/// transport markers. The same original argv reaches a Lab child, where its
/// resolved execution identity is recorded independently.
pub fn capture(cli: &Cli, normalized_args: &[String]) {
    let mut slot = captured_storage()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        *slot = Some(build(cli, normalized_args));
    }
}

/// Return the command provenance captured for this process, if any. Older
/// callers and imported observations remain valid without this optional field.
pub fn captured() -> Option<Value> {
    captured_storage().read().ok().and_then(|slot| slot.clone())
}

fn build(cli: &Cli, normalized_args: &[String]) -> Value {
    let execution = homeboy::core::resource_policy_context::lab_execution_runner_id()
        .map(|runner_id| ("lab", Some(runner_id)))
        .unwrap_or(("controller", None));
    build_with_execution(cli, normalized_args, execution.0, execution.1)
}

fn build_with_execution(
    cli: &Cli,
    normalized_args: &[String],
    location: &str,
    runner_id: Option<String>,
) -> Value {
    let argv = redact_execution_argv(normalized_args);
    let rerun_command = homeboy::core::redaction::redact_argv_shell_display(&argv);
    let placement = placement_name(cli.placement);
    let decision_origin = match (cli.runner.is_some(), cli.placement, location) {
        (true, _, _) | (_, Placement::Local | Placement::Lab, _) => "explicit",
        (_, Placement::LabOrLocal, "controller") => "fallback",
        _ => "automatic",
    };

    json!({
        "schema": SCHEMA,
        "operator_intent": {
            "argv": argv,
            "rerun_command": rerun_command,
            "placement": placement,
            "runner_id": cli.runner,
            "global_flags": {
                "detach_after_handoff": cli.detach_after_handoff,
                "allow_dirty_lab_workspace": cli.allow_dirty_lab_workspace,
                "skip_deps_hydration": cli.skip_deps_hydration,
                "runner_env": cli.runner_env.iter().map(|value| redact_env_assignment(value, &homeboy::core::redaction::RedactionPolicy::default())).collect::<Vec<_>>(),
                "lab_env_json": cli.lab_env_json.as_ref().map(|value| redact_json_env(value, &homeboy::core::redaction::RedactionPolicy::default())),
            },
        },
        "resolved_execution": {
            "location": location,
            "runner_id": runner_id,
        },
        "resource_policy": {
            "decision_origin": decision_origin,
            "preflight": crate::commands::utils::resource_policy::captured_context()
                .as_ref()
                .map(crate::commands::utils::resource_policy::resource_policy_context_to_json),
        },
    })
}

fn redact_execution_argv(args: &[String]) -> Vec<String> {
    let policy = homeboy::core::redaction::RedactionPolicy::default();
    let mut redacted = homeboy::core::redaction::redact_argv(args);
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

#[cfg(test)]
pub fn reset_captured_for_test() {
    if let Ok(mut slot) = captured_storage().write() {
        *slot = None;
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
        build(&cli, &argv)
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
    fn records_resolved_lab_runner_separately_from_lab_intent() {
        let args = ["homeboy", "--placement", "lab", "review"];
        let argv = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let cli = Cli::try_parse_from(&argv).expect("parse CLI");
        let value = build_with_execution(&cli, &argv, "lab", Some("runner-a".to_string()));

        assert_eq!(value["operator_intent"]["placement"], "lab");
        assert_eq!(value["resolved_execution"]["location"], "lab");
        assert_eq!(value["resolved_execution"]["runner_id"], "runner-a");
        assert_eq!(value["resource_policy"]["decision_origin"], "explicit");
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
}
