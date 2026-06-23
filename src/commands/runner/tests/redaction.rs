use std::collections::BTreeMap;

use super::super::dispatch::map_registry;
use super::super::types::{
    RunnerEnvDiagnostics, RunnerEnvOutput, RunnerOutput, RunnerSecretEnvReferenceOutput,
    REDACTED_ENV_VALUE,
};
use super::runner_with_env;

#[test]
fn registry_entity_output_redacts_runner_env_values() {
    let (output, exit_code) = map_registry(Ok((
        RunnerOutput {
            command: "runner.show".to_string(),
            entity: Some(runner_with_env("lab")),
            ..Default::default()
        },
        0,
    )))
    .expect("map output");

    assert_eq!(exit_code, 0);
    let value = serde_json::to_value(output).expect("serialize output");
    assert_eq!(value["variant"], "show");
    assert_eq!(
        value["entity"]["env"]["OPENCODE_API_KEY"],
        REDACTED_ENV_VALUE
    );
    assert_eq!(
        value["entity"]["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
        "https://artifacts.example.test"
    );
    assert!(!value.to_string().contains("secret-token"));
}

#[test]
fn registry_list_output_redacts_runner_env_values() {
    let (output, _) = map_registry(Ok((
        RunnerOutput {
            command: "runner.list".to_string(),
            entities: vec![runner_with_env("lab")],
            ..Default::default()
        },
        0,
    )))
    .expect("map output");

    let value = serde_json::to_value(output).expect("serialize output");
    assert_eq!(value["variant"], "list");
    assert_eq!(
        value["entities"][0]["env"]["OPENCODE_API_KEY"],
        REDACTED_ENV_VALUE
    );
    assert_eq!(
        value["entities"][0]["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
        "https://artifacts.example.test"
    );
    assert!(!value.to_string().contains("secret-token"));
}

#[test]
fn runner_env_output_redacts_values_by_default() {
    let output = RunnerEnvOutput {
        variant: "env",
        command: "runner.env".to_string(),
        runner_id: "lab".to_string(),
        source: "runner_job_env".to_string(),
        values_redacted: true,
        env: BTreeMap::from([("TOKEN".to_string(), REDACTED_ENV_VALUE.to_string())]),
        secret_env: BTreeMap::new(),
        diagnostics: RunnerEnvDiagnostics {
            server_shell_env: "shell".to_string(),
            runner_job_env: "runner".to_string(),
            wp_codebox: None,
        },
    };

    let value = serde_json::to_value(output).expect("serialize output");

    assert_eq!(value["command"], "runner.env");
    assert_eq!(value["variant"], "env");
    assert_eq!(value["source"], "runner_job_env");
    assert_eq!(value["values_redacted"], true);
    assert_eq!(value["env"]["TOKEN"], REDACTED_ENV_VALUE);
}

#[test]
fn runner_env_output_reports_secret_env_refs_without_values() {
    let output = RunnerEnvOutput {
        variant: "env",
        command: "runner.env".to_string(),
        runner_id: "lab".to_string(),
        source: "runner_job_env".to_string(),
        values_redacted: true,
        env: BTreeMap::from([(
            "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL".to_string(),
            REDACTED_ENV_VALUE.to_string(),
        )]),
        secret_env: BTreeMap::from([(
            "OPENAI_API_KEY".to_string(),
            RunnerSecretEnvReferenceOutput {
                env: Some("OPENAI_API_KEY".to_string()),
                file: None,
                secret: None,
                values_redacted: true,
            },
        )]),
        diagnostics: RunnerEnvDiagnostics {
            server_shell_env: "shell".to_string(),
            runner_job_env: "runner".to_string(),
            wp_codebox: None,
        },
    };

    let value = serde_json::to_value(output).expect("serialize output");

    assert_eq!(
        value["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
        REDACTED_ENV_VALUE
    );
    assert_eq!(
        value["secret_env"]["OPENAI_API_KEY"]["env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        value["secret_env"]["OPENAI_API_KEY"]["values_redacted"],
        true
    );
    assert!(!value.to_string().contains("dummy-secret"));
}

#[test]
fn runner_env_output_reports_secret_store_refs_without_values() {
    let output = RunnerEnvOutput {
        variant: "env",
        command: "runner.env".to_string(),
        runner_id: "lab".to_string(),
        source: "runner_job_env".to_string(),
        values_redacted: true,
        env: BTreeMap::new(),
        secret_env: BTreeMap::from([(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            RunnerSecretEnvReferenceOutput {
                env: None,
                file: None,
                secret: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()),
                values_redacted: true,
            },
        )]),
        diagnostics: RunnerEnvDiagnostics {
            server_shell_env: "shell".to_string(),
            runner_job_env: "runner".to_string(),
            wp_codebox: None,
        },
    };

    let value = serde_json::to_value(output).expect("serialize output");

    assert_eq!(
        value["secret_env"]["HOMEBOY_PREVIEW_TUNNEL_TOKEN"]["secret"],
        "HOMEBOY_PREVIEW_TUNNEL_TOKEN"
    );
    assert_eq!(
        value["secret_env"]["HOMEBOY_PREVIEW_TUNNEL_TOKEN"]["values_redacted"],
        true
    );
    assert!(!value.to_string().contains("dummy-secret"));
}
