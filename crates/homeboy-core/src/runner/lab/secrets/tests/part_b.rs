#![cfg(test)]

use super::*;
use crate::command_invocation::CommandInvocation;
use crate::runner::RunnerKind;
use crate::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

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
fn declared_agent_task_controller_run_from_spec_includes_dispatch_provider_config_secrets() {
    let names = declared_agent_task_secret_env(&[
        "homeboy".to_string(),
        "agent-task".to_string(),
        "controller".to_string(),
        "run-from-spec".to_string(),
        "@controller-spec.json".to_string(),
        "--dispatch-provider-config".to_string(),
        serde_json::json!({
            "provider": "example",
            "secret_env": ["HOMEBOY_CONTROLLER_PROVIDER_TOKEN"]
        })
        .to_string(),
    ]);

    assert_eq!(names, vec!["HOMEBOY_CONTROLLER_PROVIDER_TOKEN".to_string()]);
}

#[test]
fn lab_secret_env_handoff_plan_hydrates_controller_dispatch_provider_config_secret() {
    let _secret = RemovedEnvVar::new("HOMEBOY_CONTROLLER_PROVIDER_TOKEN");
    std::env::set_var(
        "HOMEBOY_CONTROLLER_PROVIDER_TOKEN",
        "controller-secret-value",
    );

    let plan = build_lab_secret_env_handoff_plan(
        &[LabSecretEnvSource::AgentTask],
        &[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "controller".to_string(),
            "run-from-spec".to_string(),
            "@controller-spec.json".to_string(),
            "--dispatch-provider-config".to_string(),
            serde_json::json!({
                "provider": "example",
                "secret_env": ["HOMEBOY_CONTROLLER_PROVIDER_TOKEN"]
            })
            .to_string(),
        ],
        HashMap::new(),
    )
    .expect("controller dispatch provider config secret should hydrate");

    assert_eq!(
        plan.secret_env_names,
        vec!["HOMEBOY_CONTROLLER_PROVIDER_TOKEN".to_string()]
    );
    assert_eq!(
        plan.env_delta
            .get("HOMEBOY_CONTROLLER_PROVIDER_TOKEN")
            .map(String::as_str),
        Some("controller-secret-value")
    );
    assert_eq!(
        plan.env_delta,
        plan.secret_env_plan
            .materialize([(
                "HOMEBOY_CONTROLLER_PROVIDER_TOKEN".to_string(),
                "controller-secret-value".to_string(),
            )])
            .into_iter()
            .collect::<HashMap<_, _>>()
    );
    assert!(plan
        .diagnostics
        .to_string()
        .contains("HOMEBOY_CONTROLLER_PROVIDER_TOKEN"));
    assert!(!plan
        .diagnostics
        .to_string()
        .contains("controller-secret-value"));
}

#[test]
fn hydrate_agent_task_secret_env_fails_missing_controller_dispatch_provider_config_secret() {
    let _secret = RemovedEnvVar::new("HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN");
    crate::test_support::with_isolated_home(|_| {
        let mut env = HashMap::new();

        let err = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "controller".to_string(),
                "run-from-spec".to_string(),
                "@controller-spec.json".to_string(),
                "--dispatch-provider-config".to_string(),
                serde_json::json!({
                    "provider": "example",
                    "secret_env": ["HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN"]
                })
                .to_string(),
            ],
            &mut env,
        )
        .expect_err("missing controller dispatch provider config secret should fail");

        assert_eq!(err.details["field"].as_str(), Some("secret-env"));
        assert!(err
            .message
            .contains("HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN"));
        assert!(err.message.contains("missing"));
        assert!(err.details.to_string().contains("agent-task auth map-env"));
    });
}

#[test]
fn declared_agent_task_controller_dispatch_provider_defaults_include_controller_sources() {
    let provider = fixture_provider_with_example_defaults();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "controller".to_string(),
        "run-from-spec".to_string(),
        "@controller-spec.json".to_string(),
        "--dispatch-backend".to_string(),
        "sample-runtime".to_string(),
        "--dispatch-provider-config".to_string(),
        serde_json::json!({ "provider": "example-oauth" }).to_string(),
    ];

    let names = declared_agent_task_controller_secret_env_with_providers(
        &args,
        std::slice::from_ref(&provider),
    )
    .expect("dispatch provider config should parse");
    let sources = declared_agent_task_controller_secret_sources_with_providers(
        &args,
        1,
        std::slice::from_ref(&provider),
    )
    .expect("dispatch provider config sources should parse");

    assert!(names.contains(&"EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()));
    assert_eq!(
        sources
            .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
            .and_then(|source| source.path.as_deref()),
        Some("~/.example-provider/auth.json")
    );
}

#[test]
fn hydrate_agent_task_secret_env_fails_missing_provider_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing_path = temp.path().join("missing-provider-config.json");
    let mut env = HashMap::new();

    let err = hydrate_agent_task_secret_env(
        &[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            format!("@{}", missing_path.display()),
        ],
        &mut env,
    )
    .expect_err("missing provider config should fail secret discovery");

    assert_eq!(err.details["field"].as_str(), Some("provider-config"));
    assert_eq!(
        err.details["id"].as_str(),
        Some(format!("@{}", missing_path.display()).as_str())
    );
    assert!(err.message.contains("failed to read provider config"));
}

#[test]
fn hydrate_agent_task_secret_env_fails_invalid_provider_config_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("provider-config.json");
    std::fs::write(&config_path, "{not-json").expect("write invalid config");
    let mut env = HashMap::new();

    let err = hydrate_agent_task_secret_env(
        &[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            format!("@{}", config_path.display()),
        ],
        &mut env,
    )
    .expect_err("invalid provider config JSON should fail secret discovery");

    assert_eq!(err.details["field"].as_str(), Some("provider-config"));
    assert_eq!(
        err.details["id"].as_str(),
        Some(format!("@{}", config_path.display()).as_str())
    );
    assert!(err.message.contains("failed to parse provider config JSON"));
}

#[test]
fn hydrate_agent_task_secret_env_fails_non_object_provider_config() {
    let mut env = HashMap::new();

    let err = hydrate_agent_task_secret_env(
        &[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            "[]".to_string(),
        ],
        &mut env,
    )
    .expect_err("non-object provider config should fail secret discovery");

    assert_eq!(err.details["field"].as_str(), Some("provider-config"));
    assert!(err
        .message
        .contains("provider config must be a JSON object"));
}

#[test]
fn declared_agent_task_cook_provider_defaults_include_controller_sources() {
    let provider = fixture_provider_with_example_defaults();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--backend".to_string(),
        "sample-runtime".to_string(),
        "--provider-config".to_string(),
        serde_json::json!({ "provider": "example-oauth" }).to_string(),
    ];

    let names = declared_agent_task_controller_secret_env_with_providers(
        &args,
        std::slice::from_ref(&provider),
    )
    .expect("provider config should parse");
    let sources = declared_agent_task_controller_secret_sources_with_providers(
        &args,
        1,
        std::slice::from_ref(&provider),
    )
    .expect("provider config sources should parse");

    assert!(names.contains(&"EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()));
    let source = sources
        .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
        .expect("example provider default source discovered for cook");
    assert_eq!(source.source, "json-file");
    assert_eq!(
        source.path.as_deref(),
        Some("~/.example-provider/auth.json")
    );
    assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
}

#[test]
fn declared_agent_task_providers_still_include_provider_default_sources() {
    let provider = fixture_provider_with_example_defaults();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "providers".to_string(),
        "--secret-env".to_string(),
        "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
    ];

    let sources = declared_agent_task_controller_secret_sources_with_providers(
        &args,
        1,
        std::slice::from_ref(&provider),
    )
    .expect("provider default sources should parse");

    assert!(sources.contains_key("EXAMPLE_PROVIDER_ACCESS_TOKEN"));
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
                        "backend": "sample-runtime",
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
                        "backend": "sample-runtime",
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
                        "backend": "sample-runtime",
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

#[test]
fn hydrate_agent_task_secret_env_resolves_provider_default_run_plan_secrets_on_controller() {
    let _access_token_env = RemovedEnvVar::new("EXAMPLE_PROVIDER_ACCESS_TOKEN");
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        std::fs::write(
            &auth_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "controller-access-token"
                }
            })
            .to_string(),
        )
        .expect("write auth json");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "provider-default-controller-secret-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "provider-task",
                        "executor": {
                            "backend": "sample-runtime",
                            "config": {
                                "provider": "example-oauth"
                            }
                        },
                        "instructions": "Use provider credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");
        let provider = fixture_provider_with_example_defaults_at(&auth_path.display().to_string());
        let mut env = HashMap::new();

        let diagnostics = hydrate_agent_task_secret_env_with_providers(
            &run_plan_args(&plan_path),
            &mut env,
            std::slice::from_ref(&provider),
        )
        .expect("provider default source should resolve on controller");

        assert!(matches!(
            env.get("EXAMPLE_PROVIDER_ACCESS_TOKEN").map(String::as_str),
            Some("controller-access-token")
        ));
        assert_eq!(
            diagnostics["runner_deferred_secret_env"],
            serde_json::json!([])
        );
        assert_eq!(
            diagnostics["secret_env"],
            serde_json::json!([
                {
                    "name": "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                    "configured": true,
                    "source": "json-file"
                }
            ])
        );
        assert!(!diagnostics.to_string().contains("controller-access-token"));
    });
}

#[test]
fn preflight_agent_task_runner_secret_env_fails_missing_runner_ref() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan_path = temp.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "runner-secret-preflight-plan",
            "tasks": [
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "runner-owned",
                    "executor": {
                        "backend": "example",
                        "secret_env": ["HOMEBOY_RUNNER_MISSING_SECRET_TEST"]
                    },
                    "instructions": "Use runner-owned credentials."
                }
            ]
        })
        .to_string(),
    )
    .expect("write plan");

    let runner = fixture_runner(HashMap::new());
    let args = run_plan_args(&plan_path);
    let secret_env_plan = secret_env_plan_from_args(&args);
    let err = preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &runner,
        &args,
        &HashMap::new(),
        &secret_env_plan,
    )
    .expect_err("missing runner secret refs should fail before dispatch");

    assert_eq!(err.details["field"].as_str(), Some("secret-env"));
    assert!(err.message.contains("HOMEBOY_RUNNER_MISSING_SECRET_TEST"));
}

#[test]
fn preflight_agent_task_runner_secret_env_accepts_runner_ref() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan_path = temp.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "runner-secret-preflight-plan",
            "tasks": [
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "runner-owned",
                    "executor": {
                        "backend": "example",
                        "secret_env": ["HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST"]
                    },
                    "instructions": "Use runner-owned credentials."
                }
            ]
        })
        .to_string(),
    )
    .expect("write plan");

    let runner = fixture_runner(HashMap::from([(
        "HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST".to_string(),
        RunnerSecretEnvRef {
            env: Some("HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST".to_string()),
            file: None,
            secret: None,
        },
    )]));

    let args = run_plan_args(&plan_path);
    let secret_env_plan = secret_env_plan_from_args(&args);
    preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &runner,
        &args,
        &HashMap::new(),
        &secret_env_plan,
    )
    .expect("configured runner secret refs should pass");
}

#[test]
fn preflight_agent_task_runner_secret_env_accepts_request_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan_path = temp.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "runner-secret-preflight-plan",
            "tasks": [
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "runner-owned",
                    "executor": {
                        "backend": "example",
                        "secret_env": ["HOMEBOY_RUNNER_INJECTED_SECRET_TEST"]
                    },
                    "instructions": "Use request-injected credentials."
                }
            ]
        })
        .to_string(),
    )
    .expect("write plan");

    let runner = fixture_runner(HashMap::new());
    let env = HashMap::from([(
        "HOMEBOY_RUNNER_INJECTED_SECRET_TEST".to_string(),
        "redacted-test-value".to_string(),
    )]);

    let args = run_plan_args(&plan_path);
    let secret_env_plan = secret_env_plan_from_args(&args);
    preflight_agent_task_runner_secret_env_plan("lab-a", &runner, &args, &env, &secret_env_plan)
        .expect("request-injected secret env should pass");
}

#[test]
fn preflight_agent_task_runner_secret_env_dedupes_shared_missing_names_across_tasks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan_path = temp.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "shared-missing-secret-plan",
            "tasks": [
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "idea",
                    "executor": {
                        "backend": "example",
                        "secret_env": [
                            "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                            "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                        ]
                    },
                    "instructions": "Generate an idea."
                },
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "design",
                    "executor": {
                        "backend": "example",
                        "secret_env": [
                            "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                            "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                        ]
                    },
                    "instructions": "Design the idea."
                },
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "build",
                    "executor": {
                        "backend": "example",
                        "secret_env": [
                            "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                            "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                        ]
                    },
                    "instructions": "Build the idea."
                }
            ]
        })
        .to_string(),
    )
    .expect("write plan");

    let runner = fixture_runner(HashMap::new());
    let args = run_plan_args(&plan_path);
    let secret_env_plan = secret_env_plan_from_args(&args);
    let err = preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &runner,
        &args,
        &HashMap::new(),
        &secret_env_plan,
    )
    .expect_err("missing runner secret refs should fail before dispatch");

    // The operator-facing message must list each missing name exactly once,
    // preserving first-seen order, even though three tasks declare them.
    let problem = err.details["problem"]
        .as_str()
        .expect("problem detail string");
    assert!(err.message.ends_with(
        "missing required agent-task runner secret env on runner `lab-a`: \
HOMEBOY_SHARED_MISSING_SECRET_TEST, HOMEBOY_OTHER_MISSING_SECRET_TEST"
    ));
    assert_eq!(
        problem,
        "missing required agent-task runner secret env on runner `lab-a`: \
HOMEBOY_SHARED_MISSING_SECRET_TEST, HOMEBOY_OTHER_MISSING_SECRET_TEST"
    );
    // No repeats in either prose field.
    assert_eq!(
        err.message
            .matches("HOMEBOY_SHARED_MISSING_SECRET_TEST")
            .count(),
        1
    );
    assert_eq!(
        problem
            .matches("HOMEBOY_SHARED_MISSING_SECRET_TEST")
            .count(),
        1
    );

    // Per-task provenance is reported in a separate structured field.
    let required_by_tasks = err.details["required_by_tasks"]
        .as_object()
        .expect("required_by_tasks object");
    let shared_tasks = required_by_tasks["HOMEBOY_SHARED_MISSING_SECRET_TEST"]
        .as_array()
        .expect("shared name task list");
    let shared_task_ids: Vec<&str> = shared_tasks.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(shared_task_ids, vec!["idea", "design", "build"]);
    let other_task_ids: Vec<&str> = required_by_tasks["HOMEBOY_OTHER_MISSING_SECRET_TEST"]
        .as_array()
        .expect("other name task list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(other_task_ids, vec!["idea", "design", "build"]);
}
