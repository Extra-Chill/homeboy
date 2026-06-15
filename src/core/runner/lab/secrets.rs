//! Command-specific secret hydration for Lab offload.
//!
//! Splits agent-task vs trace secret discovery from the offload orchestrator so
//! that the secret env contract for each command family stays close to its
//! parser. Both entry points (`hydrate_agent_task_secret_env`,
//! `hydrate_trace_secret_env`) mutate the runner exec environment in place and
//! return diagnostic metadata that is embedded into the Lab offload envelope.

use std::collections::HashMap;

use crate::core::agent_tasks::scheduler::AgentTaskPlan;
use crate::core::agent_tasks::secrets as agent_task_secrets;
use crate::core::{config, Error, Result};

use super::args_util::{non_empty_arg, subcommand_index};

pub(super) fn hydrate_agent_task_secret_env(
    args: &[String],
    env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_agent_task_controller_secret_env(args);
    let mut runner_deferred_names = declared_agent_task_run_plan_secret_env(args);
    runner_deferred_names.sort();
    runner_deferred_names.dedup();
    if names.is_empty() && runner_deferred_names.is_empty() {
        return Ok(serde_json::json!({
            "schema": "homeboy/lab-agent-task-secret-env/v1",
            "secret_env": [],
            "runner_deferred_secret_env": [],
        }));
    }

    if !names.is_empty() {
        let resolved = agent_task_secrets::resolve_secret_env(&names).map_err(|error| {
            Error::validation_invalid_argument(
                "secret-env",
                error.message,
                None,
                Some(vec![
                    "Configure provider secrets with Homeboy's global agent-task secret config, for example `homeboy agent-task auth map-env` or `homeboy agent-task auth set-keychain`.".to_string(),
                ]),
            )
        })?;
        for (name, value) in resolved {
            env.insert(name, value);
        }
    }

    Ok(serde_json::json!({
        "schema": "homeboy/lab-agent-task-secret-env/v1",
        "secret_env": agent_task_secrets::secret_env_status(&names),
        "runner_deferred_secret_env": runner_deferred_names
            .into_iter()
            .map(|name| serde_json::json!({
                "name": name,
                "source": "runner",
            }))
            .collect::<Vec<_>>(),
    }))
}

pub(super) fn hydrate_trace_secret_env(
    args: &[String],
    env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_trace_secret_env(args);
    if names.is_empty() {
        return Ok(crate::core::trace_secrets::empty_status());
    }

    let project_id = trace_project_id_from_args(args);
    let (resolved, statuses) =
        crate::core::trace_secrets::resolve_secret_env(&names, project_id.as_deref())?;
    for (name, value) in resolved {
        env.insert(name, value);
    }

    Ok(crate::core::trace_secrets::status_metadata(statuses))
}

pub(crate) fn declared_agent_task_secret_env(args: &[String]) -> Vec<String> {
    let mut names = declared_agent_task_controller_secret_env(args);
    names.extend(declared_agent_task_run_plan_secret_env(args));
    names.sort();
    names.dedup();
    names
}

fn declared_agent_task_controller_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "agent-task").is_none() {
        return Vec::new();
    }

    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        if arg == "--provider-config" {
            if let Some(spec) = args.get(index + 1) {
                names.extend(declared_provider_config_secret_env(spec));
            }
            index += 2;
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--provider-config=") {
            names.extend(declared_provider_config_secret_env(spec));
        }
        index += 1;
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn declared_trace_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "trace").is_none() {
        return Vec::new();
    }

    declared_secret_env_args(args)
}

pub(super) fn trace_project_id_from_args(args: &[String]) -> Option<String> {
    let trace_index = subcommand_index(args, "trace")?;

    for index in (trace_index + 1)..args.len() {
        let arg = &args[index];
        if let Some(component) = arg.strip_prefix("--component=") {
            return non_empty_arg(component);
        }
        if arg == "--component" {
            return args.get(index + 1).and_then(|value| non_empty_arg(value));
        }
    }

    match args.get(trace_index + 1).map(String::as_str) {
        Some("compare") => args
            .get(trace_index + 2)
            .and_then(|value| non_empty_arg(value)),
        Some(command) if !command.starts_with('-') => Some(command.to_string()),
        _ => None,
    }
}

fn declared_secret_env_args(args: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        index += 1;
    }
    names.sort();
    names.dedup();
    names
}

fn declared_provider_config_secret_env(spec: &str) -> Vec<String> {
    let raw = match config::read_json_spec_to_string(spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(config) = value.as_object() else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(serde_json::Value::Array(items)) => {
                names.extend(
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string),
                );
            }
            Some(serde_json::Value::String(name)) => names.push(name.clone()),
            _ => {}
        }
    }
    names
}

fn declared_agent_task_run_plan_secret_env(args: &[String]) -> Vec<String> {
    let Some(plan_spec) = agent_task_run_plan_plan_spec(args) else {
        return Vec::new();
    };
    if plan_spec == "-" {
        return Vec::new();
    }
    let raw = match config::read_json_spec_to_string(&plan_spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let plan: AgentTaskPlan = match serde_json::from_str(&raw) {
        Ok(plan) => plan,
        Err(_) => return Vec::new(),
    };

    let mut names = Vec::new();
    for request in plan.tasks {
        names.extend(request.executor.secret_env);
        names.extend(
            request
                .executor
                .config
                .get("secret_env")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string),
        );
    }
    names
}

fn agent_task_run_plan_plan_spec(args: &[String]) -> Option<String> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--plan" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--plan=") {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_agent_task_secret_env_parses_repeated_and_equals_args() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
                "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_allows_global_flags_before_agent_task() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(names, vec!["HOMEBOY_TEST_ACCESS_TOKEN".to_string()]);
    }

    #[test]
    fn declared_agent_task_secret_env_ignores_trace_secret_env() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert!(names.is_empty());
    }

    #[test]
    fn declared_trace_secret_env_parses_repeated_and_equals_args() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env".to_string(),
            "STRIPE_PUBLISHABLE_KEY".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
            "--secret-env".to_string(),
            "STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "STRIPE_PUBLISHABLE_KEY".to_string(),
                "STRIPE_SECRET_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_trace_secret_env_allows_global_flags_before_trace() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--force-hot".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(names, vec!["STRIPE_SECRET_KEY".to_string()]);
    }

    #[test]
    fn trace_project_id_from_args_reads_compare_and_component_forms() {
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--force-hot".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare-variant".to_string(),
                "--component".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "--scenario".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
    }

    #[test]
    fn hydrate_trace_secret_env_reports_redacted_status_without_values() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=HOMEBOY_TRACE_SECRET_TEST_KEY".to_string(),
        ];
        let mut env = std::collections::HashMap::new();
        std::env::set_var("HOMEBOY_TRACE_SECRET_TEST_KEY", "sk_test_fake_not_real");

        let diagnostics = hydrate_trace_secret_env(&args, &mut env).expect("hydrate trace secret");

        assert_eq!(
            env.get("HOMEBOY_TRACE_SECRET_TEST_KEY").map(String::as_str),
            Some("sk_test_fake_not_real")
        );
        let rendered = diagnostics.to_string();
        assert!(rendered.contains("HOMEBOY_TRACE_SECRET_TEST_KEY"));
        assert!(rendered.contains("env"));
        assert!(!rendered.contains("sk_test_fake_not_real"));

        std::env::remove_var("HOMEBOY_TRACE_SECRET_TEST_KEY");
    }

    #[test]
    fn declared_agent_task_secret_env_includes_provider_config_secrets() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({
                "provider": "example",
                "secret_env": [
                    "HOMEBOY_PROVIDER_ACCESS_TOKEN",
                    "HOMEBOY_PROVIDER_REFRESH_TOKEN"
                ],
                "secretEnv": "HOMEBOY_PROVIDER_ACCOUNT_ID"
            })
            .to_string(),
            "--secret-env=OPENAI_API_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_PROVIDER_ACCESS_TOKEN".to_string(),
                "HOMEBOY_PROVIDER_ACCOUNT_ID".to_string(),
                "HOMEBOY_PROVIDER_REFRESH_TOKEN".to_string(),
                "OPENAI_API_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_includes_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["OPENAI_API_KEY"],
                            "config": {
                                "secret_env": ["GITHUB_TOKEN"]
                            }
                        },
                        "instructions": "Generate an idea."
                    },
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "design",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["OPENAI_API_KEY"]
                        },
                        "instructions": "Design the idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(
            names,
            vec!["GITHUB_TOKEN".to_string(), "OPENAI_API_KEY".to_string()]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_dedupes_cli_and_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["GITHUB_TOKEN"]
                        },
                        "instructions": "Generate an idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--secret-env".to_string(),
            "GITHUB_TOKEN".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(names, vec!["GITHUB_TOKEN".to_string()]);
    }

    #[test]
    fn hydrate_agent_task_secret_env_defers_run_plan_secrets_to_runner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "runner-owned-secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_RUNNER_ONLY_SECRET_ENV_TEST"]
                        },
                        "instructions": "Use runner-owned credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let mut env = HashMap::new();
        let diagnostics = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                format!("@{}", plan_path.display()),
            ],
            &mut env,
        )
        .expect("plan-declared secrets should not require controller auth");

        assert!(env.is_empty());
        assert_eq!(diagnostics["secret_env"].as_array().unwrap().len(), 0);
        assert_eq!(
            diagnostics["runner_deferred_secret_env"],
            serde_json::json!([
                {
                    "name": "HOMEBOY_RUNNER_ONLY_SECRET_ENV_TEST",
                    "source": "runner"
                }
            ])
        );
    }
}
