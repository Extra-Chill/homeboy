use super::super::dispatch::map_registry;
use super::super::types::{RunnerOutput, REDACTED_ENV_VALUE};
use super::runner_with_env;

#[test]
fn registry_show_and_list_outputs_redact_runner_env_values() {
    let show = RunnerOutput {
        command: "runner.show".to_string(),
        entity: Some(runner_with_env("lab")),
        ..Default::default()
    };
    let list = RunnerOutput {
        command: "runner.list".to_string(),
        entities: Some(vec![runner_with_env("lab")]),
        ..Default::default()
    };

    for (output, env_pointer) in [(show, "/entity/env"), (list, "/entities/0/env")] {
        let (output, exit_code) = map_registry(Ok((output, 0))).expect("map output");
        assert_eq!(exit_code, 0);
        let value = serde_json::to_value(output).expect("serialize output");
        let env = value.pointer(env_pointer).expect("runner env");
        assert_eq!(env["PI_API_KEY"], REDACTED_ENV_VALUE, "{env_pointer}");
        assert_eq!(
            env["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"], "https://artifacts.example.test",
            "{env_pointer}"
        );
        assert!(!value.to_string().contains("secret-token"), "{env_pointer}");
    }
}
