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
fn safe_artifact_slug_keeps_consumer_id_human_readable() {
    assert_eq!(
        safe_artifact_slug("preview consumer: sample"),
        "preview-consumer--sample"
    );
}
