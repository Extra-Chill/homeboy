use homeboy::core::component::{
    AuditConfig, ConfigKeyUsageConfig, ConfigKeyUsagePattern, ConfigKeyUsageRule,
    MutatingResourceAccessConfig, PublicRegistryExposureConfig,
};

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
        config_key_usage: ConfigKeyUsageConfig {
            rules: vec![ConfigKeyUsageRule {
                id: "generic-config".to_string(),
                exclude_path_contains: vec!["fixtures/".to_string()],
                write_patterns: vec![ConfigKeyUsagePattern {
                    pattern: "set_config".to_string(),
                    key_capture: "key".to_string(),
                    symbol_capture: None,
                }],
                accessor_patterns: vec![],
                read_patterns: vec![],
                accessor_symbol_read_patterns: vec![],
            }],
        },
        convention_exception_globs: vec!["generated/**".to_string()],
        mutating_resource_access: MutatingResourceAccessConfig {
            handler_registration_markers: vec!["route(".to_string()],
            access_helper_markers: vec!["owns_resource".to_string()],
            ..Default::default()
        },
        public_registry_exposure: PublicRegistryExposureConfig {
            route_markers: vec!["route(".to_string()],
            public_access_markers: vec!["allow_public".to_string()],
            route_context_lines: Some(12),
            ..Default::default()
        },
        ..Default::default()
    };

    config.merge(&AuditConfig {
        runtime_entrypoint_extends: vec!["RuntimeCommand".to_string(), "Job".to_string()],
        runtime_entrypoint_markers: vec!["@runtime-entrypoint".to_string(), "@queued".to_string()],
        lifecycle_path_globs: vec!["lifecycle/*.php".to_string(), "bin/*".to_string()],
        utility_suffixes: vec!["Verifier".to_string(), "Resolver".to_string()],
        config_key_usage: ConfigKeyUsageConfig {
            rules: vec![
                ConfigKeyUsageRule {
                    id: "generic-config".to_string(),
                    exclude_path_contains: vec![],
                    write_patterns: vec![],
                    accessor_patterns: vec![],
                    read_patterns: vec![],
                    accessor_symbol_read_patterns: vec![],
                },
                ConfigKeyUsageRule {
                    id: "state-config".to_string(),
                    exclude_path_contains: vec![],
                    write_patterns: vec![],
                    accessor_patterns: vec![],
                    read_patterns: vec![],
                    accessor_symbol_read_patterns: vec![],
                },
            ],
        },
        convention_exception_globs: vec!["generated/**".to_string(), "fixtures/**".to_string()],
        mutating_resource_access: MutatingResourceAccessConfig {
            handler_registration_markers: vec!["route(".to_string(), "command(".to_string()],
            access_helper_markers: vec!["owns_resource".to_string(), "can_access".to_string()],
            mutator_markers: vec!["delete(".to_string()],
            ..Default::default()
        },
        public_registry_exposure: PublicRegistryExposureConfig {
            route_markers: vec!["route(".to_string(), "endpoint(".to_string()],
            public_access_markers: vec!["allow_public".to_string()],
            raw_getter_patterns: vec!["raw_registry".to_string()],
            permission_aware_resolver_patterns: vec!["PolicyResolver".to_string()],
            route_context_lines: Some(6),
            resolver_path_contains: vec!["src/policy/".to_string()],
            resolver_same_namespace: true,
            ..Default::default()
        },
        ..Default::default()
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
    assert_eq!(config.config_key_usage.rules.len(), 2);
    assert_eq!(
        config.convention_exception_globs,
        vec!["generated/**", "fixtures/**"]
    );
    assert_eq!(
        config.mutating_resource_access.handler_registration_markers,
        vec!["route(", "command("]
    );
    assert_eq!(
        config.mutating_resource_access.access_helper_markers,
        vec!["owns_resource", "can_access"]
    );
    assert_eq!(
        config.mutating_resource_access.mutator_markers,
        vec!["delete("]
    );
    assert_eq!(
        config.public_registry_exposure.route_markers,
        vec!["route(", "endpoint("]
    );
    assert_eq!(
        config.public_registry_exposure.public_access_markers,
        vec!["allow_public"]
    );
    assert_eq!(
        config.public_registry_exposure.raw_getter_patterns,
        vec!["raw_registry"]
    );
    assert_eq!(
        config
            .public_registry_exposure
            .permission_aware_resolver_patterns,
        vec!["PolicyResolver"]
    );
    assert_eq!(config.public_registry_exposure.route_context_lines, Some(6));
    assert_eq!(
        config.public_registry_exposure.resolver_path_contains,
        vec!["src/policy/"]
    );
    assert!(config.public_registry_exposure.resolver_same_namespace);
}
