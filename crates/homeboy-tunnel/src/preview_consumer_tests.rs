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

/// #11128: the artifact directory used to fall back to a bare
/// `std::env::temp_dir()` path when the configured artifact root could not be
/// resolved. That silently discarded the configured root and wrote bytes with
/// no owner record, no pin, and no cleanup category, so nothing ever reclaimed
/// them.
mod artifacts_dir_resolution {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;

    #[test]
    fn an_explicit_override_wins_over_everything() {
        let resolved = resolve_artifacts_dir(
            Some(PathBuf::from("/explicit")),
            Some(PathBuf::from("/configured")),
            "consumer",
        )
        .expect("resolve");

        assert_eq!(resolved, PathBuf::from("/explicit"));
    }

    #[test]
    fn the_configured_directory_is_used_when_no_override_is_given() {
        let resolved = resolve_artifacts_dir(None, Some(PathBuf::from("/configured")), "consumer")
            .expect("resolve");

        assert_eq!(resolved, PathBuf::from("/configured"));
    }

    /// The default lands under the artifact root, where the artifact-root
    /// reapers can see it -- never under the process temp directory.
    #[test]
    fn the_default_directory_lives_under_the_configured_artifact_root() {
        with_isolated_home(|_home| {
            let root = homeboy_core::artifacts::root().expect("artifact root");

            let resolved =
                resolve_artifacts_dir(None, None, "preview consumer/one").expect("resolve");

            assert_eq!(
                resolved,
                root.join("preview-consumer")
                    .join(safe_artifact_slug("preview consumer/one"))
            );
            assert!(
                resolved.starts_with(&root),
                "the default must resolve under the artifact root, not beside it: {}",
                resolved.display()
            );
        });
    }
}
