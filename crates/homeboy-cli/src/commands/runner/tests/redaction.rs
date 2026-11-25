use super::super::dispatch::map_registry;
use super::super::types::{RunnerOutput, REDACTED_ENV_VALUE};
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
    assert_eq!(value["entity"]["env"]["PI_API_KEY"], REDACTED_ENV_VALUE);
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
            entities: Some(vec![runner_with_env("lab")]),
            ..Default::default()
        },
        0,
    )))
    .expect("map output");

    let value = serde_json::to_value(output).expect("serialize output");
    assert_eq!(value["variant"], "list");
    assert_eq!(
        value["entities"][0]["env"]["PI_API_KEY"],
        REDACTED_ENV_VALUE
    );
    assert_eq!(
        value["entities"][0]["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
        "https://artifacts.example.test"
    );
    assert!(!value.to_string().contains("secret-token"));
}
