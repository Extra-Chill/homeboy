use homeboy::component::AuditConfig;

#[test]
fn is_empty_reports_only_empty_rule_sets() {
    assert!(AuditConfig::default().is_empty());

    let config = AuditConfig {
        utility_suffixes: vec!["Verifier".to_string()],
        ..Default::default()
    };

    assert!(!config.is_empty());
}

#[test]
fn test_merge() {
    let mut config = AuditConfig {
        runtime_entrypoint_extends: vec!["RuntimeCommand".to_string()],
        runtime_entrypoint_markers: vec!["@runtime-entrypoint".to_string()],
        lifecycle_path_globs: vec!["lifecycle/*.php".to_string()],
        utility_suffixes: vec!["Verifier".to_string()],
        convention_exception_globs: vec!["generated/**".to_string()],
    };

    config.merge(&AuditConfig {
        runtime_entrypoint_extends: vec!["RuntimeCommand".to_string(), "Job".to_string()],
        runtime_entrypoint_markers: vec!["@runtime-entrypoint".to_string(), "@queued".to_string()],
        lifecycle_path_globs: vec!["lifecycle/*.php".to_string(), "bin/*".to_string()],
        utility_suffixes: vec!["Verifier".to_string(), "Resolver".to_string()],
        convention_exception_globs: vec!["generated/**".to_string(), "fixtures/**".to_string()],
    });

    assert_eq!(
        config.runtime_entrypoint_extends,
        vec!["RuntimeCommand", "Job"]
    );
    assert_eq!(
        config.runtime_entrypoint_markers,
        vec!["@runtime-entrypoint", "@queued"]
    );
    assert_eq!(
        config.lifecycle_path_globs,
        vec!["lifecycle/*.php", "bin/*"]
    );
    assert_eq!(config.utility_suffixes, vec!["Verifier", "Resolver"]);
    assert_eq!(
        config.convention_exception_globs,
        vec!["generated/**", "fixtures/**"]
    );
}
