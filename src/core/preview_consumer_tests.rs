use super::*;

#[test]
fn parses_public_result_url_from_configured_stdout_prefix() {
    let stdout = "Consumer ready\nPublic result URL: https://run.example.test/result\n";

    assert_eq!(
        parse_prefixed_line(stdout, "Public result URL:").as_deref(),
        Some("https://run.example.test/result")
    );
}

#[test]
fn detect_ready_url_matches_configured_stdout_prefix() {
    let config = PreviewConsumerOutputConfig {
        public_result_stdout_prefix: Some("Public result URL:".to_string()),
        ..Default::default()
    };

    assert_eq!(
        detect_ready_url(
            &config,
            "Public result URL: https://run.example.test/result"
        ),
        Some("https://run.example.test/result".to_string())
    );
    assert_eq!(detect_ready_url(&config, "unrelated log line"), None);
}

#[test]
fn detect_ready_url_without_configured_prefix_returns_none() {
    let config = PreviewConsumerOutputConfig::default();
    assert_eq!(
        detect_ready_url(
            &config,
            "Public result URL: https://run.example.test/result"
        ),
        None
    );
}

#[test]
fn default_run_mode_is_blocking() {
    assert_eq!(
        PreviewConsumerRunMode::default(),
        PreviewConsumerRunMode::Blocking
    );
}

#[test]
fn safe_artifact_slug_keeps_consumer_id_human_readable() {
    assert_eq!(
        safe_artifact_slug("preview consumer: sample"),
        "preview-consumer--sample"
    );
}
