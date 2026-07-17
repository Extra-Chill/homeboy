#![cfg(test)]

use super::*;
use crate::RunnerKind;
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

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
fn lab_secret_env_handoff_plan_reports_redacted_env_delta_and_exact_names() {
    let args = vec![
        "homeboy".to_string(),
        "trace".to_string(),
        "compare".to_string(),
        "woocommerce-gateway-stripe".to_string(),
        "real-wallet".to_string(),
        "--secret-env=HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string(),
    ];
    let mut env_delta = HashMap::new();
    env_delta.insert(
        "HOMEBOY_PUBLIC_CONTEXT".to_string(),
        "still-redacted-in-diagnostics".to_string(),
    );
    std::env::set_var("HOMEBOY_TRACE_HANDOFF_SECRET_TEST", "trace-secret-value");

    let plan = build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::Trace], &args, env_delta)
        .expect("handoff plan");

    assert_eq!(
        plan.env_delta
            .get("HOMEBOY_TRACE_HANDOFF_SECRET_TEST")
            .map(String::as_str),
        Some("trace-secret-value")
    );
    assert_eq!(
        plan.secret_env_names,
        vec!["HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string()]
    );
    assert_eq!(
        plan.secret_env_plan
            .public_env
            .get("HOMEBOY_PUBLIC_CONTEXT"),
        Some(&"still-redacted-in-diagnostics".to_string())
    );
    assert!(plan.runner_deferred_secret_env.is_empty());
    let materialized = plan.secret_env_plan.materialize([(
        "HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string(),
        "trace-secret-value".to_string(),
    )]);
    assert_eq!(
        plan.env_delta,
        materialized.into_iter().collect::<HashMap<_, _>>()
    );
    let rendered = plan.diagnostics.to_string();
    assert!(rendered.contains("homeboy/lab-secret-env-handoff/v1"));
    assert!(rendered.contains("HOMEBOY_TRACE_HANDOFF_SECRET_TEST"));
    assert!(rendered.contains("HOMEBOY_PUBLIC_CONTEXT"));
    assert!(!rendered.contains("trace-secret-value"));
    assert!(!rendered.contains("still-redacted-in-diagnostics"));
    let entries: Vec<SecretEnvHandoffEntry> =
        serde_json::from_value(plan.diagnostics["entries"].clone()).expect("typed entries");
    assert_eq!(
        entries,
        vec![
            SecretEnvHandoffEntry {
                name: "HOMEBOY_PUBLIC_CONTEXT".to_string(),
                owner: "controller".to_string(),
                source: "env-delta".to_string(),
                destination: "runner".to_string(),
                status: "forwarded".to_string(),
                remediation: None,
            },
            SecretEnvHandoffEntry {
                name: "HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string(),
                owner: "controller".to_string(),
                source: "env".to_string(),
                destination: "runner".to_string(),
                status: "forwarded".to_string(),
                remediation: None,
            },
        ]
    );

    std::env::remove_var("HOMEBOY_TRACE_HANDOFF_SECRET_TEST");
}

#[test]
fn lab_secret_env_handoff_plan_consumes_declared_sources_only() {
    let args = vec![
        "homeboy".to_string(),
        "trace".to_string(),
        "compare".to_string(),
        "woocommerce-gateway-stripe".to_string(),
        "real-wallet".to_string(),
        "--secret-env=HOMEBOY_UNDECLARED_TRACE_SECRET_TEST".to_string(),
    ];
    let _secret = RemovedEnvVar::new("HOMEBOY_UNDECLARED_TRACE_SECRET_TEST");
    std::env::set_var("HOMEBOY_UNDECLARED_TRACE_SECRET_TEST", "trace-secret-value");

    let plan = build_lab_secret_env_handoff_plan(&[], &args, HashMap::new()).expect("handoff plan");

    assert!(plan.secret_env_names.is_empty());
    assert!(plan.env_delta.is_empty());
    assert!(plan.runner_deferred_secret_env.is_empty());
}

#[test]
fn lab_secret_env_handoff_plan_reports_runner_deferred_requirements() {
    let args = vec![
        "homeboy".to_string(),
        "tunnel".to_string(),
        "preview-client".to_string(),
        "start".to_string(),
        "--ingress".to_string(),
        "https://preview-broker.example.test".to_string(),
        "--token-env=HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string(),
    ];

    let plan =
        build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::Tunnel], &args, HashMap::new())
            .expect("handoff plan");

    assert!(plan.env_delta.is_empty());
    assert_eq!(
        plan.secret_env_names,
        vec!["HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string()]
    );
    assert_eq!(
        plan.runner_deferred_secret_env,
        vec!["HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string()]
    );
    assert_eq!(
        plan.diagnostics["runner_deferred_secret_env"][0]["name"],
        "HOMEBOY_RUNNER_PREVIEW_TOKEN"
    );
    assert_eq!(
        plan.diagnostics["runner_deferred_secret_env"][0]["required"],
        true
    );
    let entries: Vec<SecretEnvHandoffEntry> =
        serde_json::from_value(plan.diagnostics["entries"].clone()).expect("typed entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "HOMEBOY_RUNNER_PREVIEW_TOKEN");
    assert_eq!(entries[0].owner, "runner");
    assert_eq!(entries[0].source, "runner");
    assert_eq!(entries[0].destination, "runner");
    assert_eq!(entries[0].status, "deferred");
    assert!(entries[0]
        .remediation
        .as_deref()
        .expect("remediation")
        .contains("runner secret_env references"));
}

#[test]
fn preflight_lab_secret_env_handoff_reports_missing_runner_deferred_secret() {
    let args = vec![
        "homeboy".to_string(),
        "tunnel".to_string(),
        "preview-client".to_string(),
        "start".to_string(),
        "--ingress".to_string(),
        "https://preview-broker.example.test".to_string(),
        "--token-env=HOMEBOY_RUNNER_PREFLIGHT_TOKEN".to_string(),
    ];
    let handoff =
        build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::Tunnel], &args, HashMap::new())
            .expect("handoff");
    let runner = fixture_runner(HashMap::new());

    let err = preflight_lab_secret_env_handoff("lab-a", Some(&runner), &HashMap::new(), &handoff)
        .expect_err("missing runner-deferred secret should fail before dispatch");

    assert!(err.message.contains("HOMEBOY_RUNNER_PREFLIGHT_TOKEN"));
    let rendered = err.details.to_string();
    assert!(rendered.contains("runner-deferred secrets"));
    assert!(rendered.contains("runner secret_env references"));
    assert!(rendered.contains("\"status\":\"missing\""));
}

#[test]
fn preflight_lab_secret_env_handoff_preserves_unknown_runner_side_status() {
    let args = vec![
        "homeboy".to_string(),
        "tunnel".to_string(),
        "preview-client".to_string(),
        "start".to_string(),
        "--ingress".to_string(),
        "https://preview-broker.example.test".to_string(),
        "--token-env=HOMEBOY_UNKNOWN_RUNNER_TOKEN".to_string(),
    ];
    let handoff =
        build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::Tunnel], &args, HashMap::new())
            .expect("handoff");

    preflight_lab_secret_env_handoff("lab-a", None, &HashMap::new(), &handoff)
        .expect("unknown runner-side secret status should remain runner-deferred");
}

#[test]
fn preflight_lab_secret_env_handoff_reports_redacted_controller_forwarded_secret() {
    let args = vec![
        "homeboy".to_string(),
        "trace".to_string(),
        "compare".to_string(),
        "woocommerce-gateway-stripe".to_string(),
        "real-wallet".to_string(),
        "--secret-env=HOMEBOY_CONTROLLER_PREFLIGHT_SECRET".to_string(),
    ];
    std::env::set_var(
        "HOMEBOY_CONTROLLER_PREFLIGHT_SECRET",
        "controller-secret-value-must-not-leak",
    );
    let handoff =
        build_lab_secret_env_handoff_plan(&[LabSecretEnvSource::Trace], &args, HashMap::new())
            .expect("handoff");
    let runner = fixture_runner(HashMap::new());

    let err = preflight_lab_secret_env_handoff("lab-a", Some(&runner), &HashMap::new(), &handoff)
        .expect_err("missing forwarded controller secret should fail before dispatch");

    let rendered = err.details.to_string();
    assert!(rendered.contains("HOMEBOY_CONTROLLER_PREFLIGHT_SECRET"));
    assert!(rendered.contains("controller-forwarded secrets"));
    assert!(rendered.contains("controller environment"));
    assert!(!rendered.contains("controller-secret-value-must-not-leak"));

    std::env::remove_var("HOMEBOY_CONTROLLER_PREFLIGHT_SECRET");
}

#[test]
fn lab_secret_env_handoff_plan_carries_canonical_secret_env_plan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan_path = temp.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "canonical-secret-env-plan",
            "tasks": [
                {
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "runner-owned",
                    "executor": {
                        "backend": "example",
                        "secret_env": ["HOMEBOY_CANONICAL_RUNNER_SECRET_TEST"]
                    },
                    "instructions": "Use runner-owned credentials."
                }
            ]
        })
        .to_string(),
    )
    .expect("write plan");

    let handoff = build_lab_secret_env_handoff_plan(
        &[LabSecretEnvSource::AgentTask],
        &run_plan_args(&plan_path),
        HashMap::new(),
    )
    .expect("handoff plan");

    assert_eq!(
        handoff.secret_env_plan.secret_env_names(),
        handoff.secret_env_names
    );
    assert_eq!(
        handoff.diagnostics["secret_env_plan"]["schema"].as_str(),
        Some(homeboy_core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA)
    );
    assert_eq!(
        handoff.runner_deferred_secret_env,
        vec!["HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string()]
    );
    let runner = fixture_runner(HashMap::from([(
        "HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string(),
        RunnerSecretEnvRef {
            env: Some("HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string()),
            file: None,
            secret: None,
        },
    )]));

    preflight_agent_task_runner_secret_env_plan(
        "lab-a",
        &runner,
        &run_plan_args(&plan_path),
        &handoff.env_delta,
        &handoff.secret_env_plan,
    )
    .expect("preflight consumes handoff secret env plan");
}

#[test]
fn declared_tunnel_secret_env_reads_preview_client_default_and_override() {
    assert_eq!(
        declared_tunnel_secret_env(&[
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
        ]),
        vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]
    );

    assert_eq!(
        declared_tunnel_secret_env(&[
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--token-env=RUNNER_PREVIEW_TOKEN".to_string(),
        ]),
        vec!["RUNNER_PREVIEW_TOKEN".to_string()]
    );
}

#[test]
fn declared_tunnel_secret_env_detects_service_preview_client_backend() {
    let names = declared_tunnel_secret_env(&[
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "tunnel".to_string(),
        "service".to_string(),
        "start".to_string(),
        "runner-preview".to_string(),
        "--command".to_string(),
        "npm run dev".to_string(),
        "--public-tunnel-backend".to_string(),
        "command".to_string(),
        "--public-tunnel-command".to_string(),
        "homeboy tunnel preview-client start --ready-stdout".to_string(),
    ]);

    assert_eq!(names, vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]);
}

#[test]
fn hydrate_tunnel_secret_env_reports_runner_deferred_names_without_values() {
    let args = vec![
        "homeboy".to_string(),
        "tunnel".to_string(),
        "preview-client".to_string(),
        "start".to_string(),
        "--ingress".to_string(),
        "https://preview-broker.example.test".to_string(),
        "--public-host".to_string(),
        "preview.example.test".to_string(),
        "--local-origin".to_string(),
        "http://127.0.0.1:8888".to_string(),
    ];
    let mut env = std::collections::HashMap::new();

    let diagnostics = hydrate_tunnel_secret_env(&args, &mut env).expect("hydrate tunnel secret");

    assert!(env.is_empty());
    let rendered = diagnostics.to_string();
    assert!(rendered.contains("homeboy/lab-tunnel-secret-env/v1"));
    assert!(rendered.contains("HOMEBOY_PREVIEW_TUNNEL_TOKEN"));
    assert!(rendered.contains("runner"));
}
