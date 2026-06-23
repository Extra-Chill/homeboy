use super::*;

#[test]
fn provider_file_secret_source_provisions_group_json_file_sources_without_values() {
    let sources = HashMap::from([
        (
            "PROVIDER_ACCESS_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.access_token"),
        ),
        (
            "PROVIDER_REFRESH_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
        ),
        (
            "UNRELATED_SECRET".to_string(),
            AgentTaskSecretSource {
                source: "env".to_string(),
                env_var: Some("UNRELATED_SECRET".to_string()),
                path: None,
                scope: None,
                name: None,
                field: None,
                value: None,
            },
        ),
    ]);

    let provisions = provider_file_secret_source_provisions(
        &[
            "PROVIDER_REFRESH_TOKEN".to_string(),
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "UNRELATED_SECRET".to_string(),
        ],
        &sources,
    );

    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].path, "~/.provider/auth.json");
    assert_eq!(
        provisions[0].env_names,
        vec![
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "PROVIDER_REFRESH_TOKEN".to_string(),
        ]
    );
    let rendered = format!("{:?}", provisions);
    assert!(!rendered.contains("access-secret"));
    assert!(!rendered.contains("refresh-secret"));
}

#[test]
fn runner_secret_env_resolution_uses_provider_json_file_source_values() {
    crate::test_support::with_isolated_home(|home| {
        let provider_dir = home.path().join(".provider");
        std::fs::create_dir_all(&provider_dir).expect("provider dir");
        std::fs::write(
            provider_dir.join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "access_token": "access-secret-value",
                    "refresh_token": "refresh-secret-value"
                }
            })
            .to_string(),
        )
        .expect("auth json");
        let sources = HashMap::from([
            (
                "PROVIDER_ACCESS_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.access_token"),
            ),
            (
                "PROVIDER_REFRESH_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
            ),
        ]);

        let resolved = resolve_runner_secret_env_for_command_with_fallbacks(
            &HashMap::new(),
            &[
                "PROVIDER_ACCESS_TOKEN".to_string(),
                "PROVIDER_REFRESH_TOKEN".to_string(),
            ],
            &HashMap::new(),
            &sources,
        )
        .expect("provider sources resolve on runner");

        assert_eq!(
            resolved.get("PROVIDER_ACCESS_TOKEN"),
            Some(&"access-secret-value".to_string())
        );
        assert_eq!(
            resolved.get("PROVIDER_REFRESH_TOKEN"),
            Some(&"refresh-secret-value".to_string())
        );
    });
}

#[test]
fn provider_file_secret_source_error_is_early_clear_and_redacted() {
    let provision = ProviderFileSecretSourceProvision {
        path: "~/.provider/auth.json".to_string(),
        env_names: vec![
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "PROVIDER_REFRESH_TOKEN".to_string(),
        ],
    };

    let err = provider_file_secret_source_error(
        "homeboy-lab",
        &provision,
        "controller credential source is not readable".to_string(),
    );

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("homeboy-lab"));
    assert!(err.message.contains("PROVIDER_ACCESS_TOKEN"));
    assert!(err
        .message
        .contains("controller credential source is not readable"));
    assert!(err.details["tried"]
        .as_array()
        .is_some_and(|hints| hints.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Refresh the provider credentials")))));
    let rendered = format!("{} {:?} {:?}", err.message, err.details, err.hints);
    assert!(!rendered.contains("access-secret-value"));
    assert!(!rendered.contains("refresh-secret-value"));
}
