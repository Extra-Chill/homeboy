use serde::{Deserialize, Serialize};

mod command_status;
mod config_key_usage;
mod detector_profile;
mod exposure;
mod known_symbols;
mod language_grammar;
mod remote_execution;
mod source_policy;
mod test_wiring;
mod thin_command_adapter;

pub use command_status::{CommandStatusContractConfig, CommandStatusContractScenario};
pub use config_key_usage::{ConfigKeyUsageConfig, ConfigKeyUsagePattern, ConfigKeyUsageRule};
pub use detector_profile::{DetectorProfileConfig, VersionSource};
pub use exposure::{
    DuplicationDetectorConfig, PublicRegistryExposureConfig, RedirectValidationConfig,
};
pub use known_symbols::{
    KnownSymbolBootstrapPathProvider, KnownSymbolEntry, KnownSymbolHeaderVersionProvider,
    KnownSymbolKind, KnownSymbolManifestPackageProvider, KnownSymbolSourceScanConfig,
    KnownSymbolVersionedEntry, KnownSymbolsConfig,
};
pub use language_grammar::{grammar_for_extension, LanguageGrammar};
pub use remote_execution::{ArtifactPortabilityConfig, RemoteExecutionSafetyConfig};
pub use source_policy::{
    ConventionTagGlob, CoreBoundaryLeakConfig, MutatingResourceAccessConfig, RequestedDetectorRule,
    RequestedDetectorRuleBody, RequiredRegexScope, SourcePolicyMatchMode, SourcePolicyRule,
    SourcePolicyRuleBody, SourcePolicyTerm,
};
pub use test_wiring::{TestWiringConfig, TestWiringPolicy};
pub use thin_command_adapter::{ThinCommandAdapterConfig, ThinCommandAdapterMarkerGroup};

#[cfg(test)]
#[path = "../../../../tests/core/component/audit_test.rs"]
mod audit_test;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditConfig {
    /// Class/base names whose public methods are invoked by a runtime dispatcher.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_entrypoint_extends: Vec<String>,
    /// Source markers that indicate public methods are runtime-dispatched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_entrypoint_markers: Vec<String>,
    /// Paths whose guards run outside normal production runtime assumptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_path_globs: Vec<String>,
    /// Extension-owned regexes matched against nearby guard comments. Core only
    /// applies the patterns; extensions own the contextual language.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dead_guard_context_comment_patterns: Vec<String>,
    /// Type suffixes that mark convention outliers as intentional utilities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub utility_suffixes: Vec<String>,
    /// Files exempt from convention outlier checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convention_exception_globs: Vec<String>,
    /// Component-owned path rules that attach opaque tags before convention grouping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convention_tag_globs: Vec<ConventionTagGlob>,
    /// Symbols that are known to exist when component metadata proves a runtime
    /// floor, package, or bootstrap file is present.
    #[serde(default, skip_serializing_if = "KnownSymbolsConfig::is_empty")]
    pub known_symbols: KnownSymbolsConfig,
    /// Extension-owned text detector rules that emit audit findings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_detectors: Vec<RequestedDetectorRule>,
    /// Component-owned source policy rules for generic architecture boundaries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_policies: Vec<SourcePolicyRule>,
    /// Configurable ecosystem-term checks for core-owned source boundaries.
    #[serde(default, skip_serializing_if = "CoreBoundaryLeakConfig::is_empty")]
    pub core_boundary_leaks: CoreBoundaryLeakConfig,
    /// Component-owned markers for mutating handler/resource-id paths that must
    /// perform an ownership/access check before mutating the resource.
    #[serde(
        default,
        skip_serializing_if = "MutatingResourceAccessConfig::is_empty"
    )]
    pub mutating_resource_access: MutatingResourceAccessConfig,
    /// Configurable checks for request-derived redirect destinations that are
    /// used before URL validation dominates the redirect sink.
    #[serde(default, skip_serializing_if = "RedirectValidationConfig::is_empty")]
    pub redirect_validation: RedirectValidationConfig,
    /// Extension-owned call-name lists used by the duplication /
    /// parallel-implementation detector to filter out language- and
    /// framework-specific noise. Core never interprets these strings; they
    /// are merged with the built-in generic floor lists.
    #[serde(default, skip_serializing_if = "DuplicationDetectorConfig::is_empty")]
    pub duplication_detector: DuplicationDetectorConfig,
    /// Configurable route/permission/getter/resolver markers for detecting
    /// public metadata endpoints that bypass a permission-aware resolver.
    #[serde(
        default,
        skip_serializing_if = "PublicRegistryExposureConfig::is_empty"
    )]
    pub public_registry_exposure: PublicRegistryExposureConfig,
    /// Component-owned regexes that correlate config-key writes, accessors, and
    /// reads. Core only matches configured captures; components own semantics.
    #[serde(default, skip_serializing_if = "ConfigKeyUsageConfig::is_empty")]
    pub config_key_usage: ConfigKeyUsageConfig,
    /// Component-owned command scenario fixtures with expected status fields.
    #[serde(default, skip_serializing_if = "CommandStatusContractConfig::is_empty")]
    pub command_status_contracts: CommandStatusContractConfig,
    /// Component-owned markers that prove remote execution dispatch sites satisfy
    /// generic safety invariants before work leaves the local machine.
    #[serde(default, skip_serializing_if = "RemoteExecutionSafetyConfig::is_empty")]
    pub remote_execution_safety: RemoteExecutionSafetyConfig,
    /// Component-owned path policy for durable artifact portability checks.
    #[serde(default, skip_serializing_if = "ArtifactPortabilityConfig::is_empty")]
    pub artifact_portability: ArtifactPortabilityConfig,
    /// Component-owned test wiring policies. Core evaluates path and marker
    /// rules without knowing the language or test harness semantics.
    #[serde(default, skip_serializing_if = "TestWiringConfig::is_empty")]
    pub test_wiring: TestWiringConfig,
    /// Ecosystem profile data for detectors that need project-specific marker,
    /// tracker, path, or version-guard catalogues.
    #[serde(default, skip_serializing_if = "DetectorProfileConfig::is_empty")]
    pub detector_profile: DetectorProfileConfig,
    /// Component-owned thin-command-adapter policy. Flags command-layer modules
    /// that accumulate orchestration/business logic instead of staying thin
    /// adapters over core services.
    #[serde(default, skip_serializing_if = "ThinCommandAdapterConfig::is_empty")]
    pub thin_command_adapter: ThinCommandAdapterConfig,
    /// Component-owned per-language grammars describing how to count top-level
    /// item declarations. Core applies them generically; the component owns all
    /// language item keywords (e.g. `fn `, `struct `, `function `, `class `).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_grammars: Vec<LanguageGrammar>,
}

impl AuditConfig {
    pub fn is_empty(&self) -> bool {
        self.runtime_entrypoint_extends.is_empty()
            && self.runtime_entrypoint_markers.is_empty()
            && self.lifecycle_path_globs.is_empty()
            && self.dead_guard_context_comment_patterns.is_empty()
            && self.utility_suffixes.is_empty()
            && self.convention_exception_globs.is_empty()
            && self.convention_tag_globs.is_empty()
            && self.known_symbols.is_empty()
            && self.requested_detectors.is_empty()
            && self.source_policies.is_empty()
            && self.core_boundary_leaks.is_empty()
            && self.mutating_resource_access.is_empty()
            && self.redirect_validation.is_empty()
            && self.duplication_detector.is_empty()
            && self.public_registry_exposure.is_empty()
            && self.config_key_usage.is_empty()
            && self.command_status_contracts.is_empty()
            && self.remote_execution_safety.is_empty()
            && self.artifact_portability.is_empty()
            && self.test_wiring.is_empty()
            && self.detector_profile.is_empty()
            && self.thin_command_adapter.is_empty()
            && self.language_grammars.is_empty()
    }

    pub fn merge(&mut self, other: &AuditConfig) {
        extend_unique(
            &mut self.runtime_entrypoint_extends,
            &other.runtime_entrypoint_extends,
        );
        extend_unique(
            &mut self.runtime_entrypoint_markers,
            &other.runtime_entrypoint_markers,
        );
        extend_unique(&mut self.lifecycle_path_globs, &other.lifecycle_path_globs);
        extend_unique(
            &mut self.dead_guard_context_comment_patterns,
            &other.dead_guard_context_comment_patterns,
        );
        extend_unique(&mut self.utility_suffixes, &other.utility_suffixes);
        extend_unique(
            &mut self.convention_exception_globs,
            &other.convention_exception_globs,
        );
        extend_unique(&mut self.convention_tag_globs, &other.convention_tag_globs);
        self.known_symbols.merge(&other.known_symbols);
        self.core_boundary_leaks.merge(&other.core_boundary_leaks);
        self.mutating_resource_access
            .merge(&other.mutating_resource_access);
        self.redirect_validation.merge(&other.redirect_validation);
        self.duplication_detector.merge(&other.duplication_detector);
        self.public_registry_exposure
            .merge(&other.public_registry_exposure);
        self.config_key_usage.merge(&other.config_key_usage);
        self.command_status_contracts
            .merge(&other.command_status_contracts);
        self.remote_execution_safety
            .merge(&other.remote_execution_safety);
        self.artifact_portability.merge(&other.artifact_portability);
        self.detector_profile.merge(&other.detector_profile);
        self.thin_command_adapter.merge(&other.thin_command_adapter);
        language_grammar::merge_language_grammars(
            &mut self.language_grammars,
            &other.language_grammars,
        );
        for rule in &other.source_policies {
            if !self
                .source_policies
                .iter()
                .any(|existing| existing.id == rule.id)
            {
                self.source_policies.push(rule.clone());
            }
        }
        self.test_wiring.merge(&other.test_wiring);
        for rule in &other.requested_detectors {
            if !self
                .requested_detectors
                .iter()
                .any(|existing| existing.id == rule.id)
            {
                self.requested_detectors.push(rule.clone());
            }
        }
    }
}

pub(crate) fn extend_unique<T: Clone + PartialEq>(target: &mut Vec<T>, source: &[T]) {
    for value in source {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn core_boundary_leak_config_marks_audit_config_non_empty() {
        let config = AuditConfig {
            core_boundary_leaks: CoreBoundaryLeakConfig {
                terms: vec!["florpstack".to_string()],
                scan_path_contains: vec!["src/core/".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn source_policies_mark_audit_config_non_empty() {
        let config = AuditConfig {
            source_policies: vec![SourcePolicyRule {
                id: "synthetic-boundary".to_string(),
                kind: "source_policy_violation".to_string(),
                severity: "warning".to_string(),
                convention: "source_policy".to_string(),
                language: None,
                file_extensions: Vec::new(),
                include_path_contains: vec!["src/core/".to_string()],
                exclude_path_contains: Vec::new(),
                allow_line_contains: Vec::new(),
                ignore_line_prefixes: Vec::new(),
                ignore_after_line_equals: Vec::new(),
                example_path_contains: Vec::new(),
                example_classification: None,
                description: "Forbidden term `{term}` at line {line}".to_string(),
                suggestion: "Move the term into component policy.".to_string(),
                rule: SourcePolicyRuleBody::ForbiddenTerms {
                    terms: vec![SourcePolicyTerm {
                        value: "florpstack".to_string(),
                        label: None,
                        match_mode: None,
                    }],
                    default_match: SourcePolicyMatchMode::Token,
                    case_insensitive: true,
                },
            }],
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn thin_command_adapter_config_marks_audit_config_non_empty() {
        let config = AuditConfig {
            thin_command_adapter: ThinCommandAdapterConfig {
                include_path_contains: vec!["src/commands/".to_string()],
                orchestration_markers: vec![ThinCommandAdapterMarkerGroup {
                    label: "process execution".to_string(),
                    patterns: vec!["Command::new".to_string()],
                    weight: 1,
                    exempt_when_line_matches: Vec::new(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn thin_command_adapter_config_requires_scope_and_markers_to_be_non_empty() {
        let convention_only = AuditConfig {
            thin_command_adapter: ThinCommandAdapterConfig {
                convention: "thin_command_adapter".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(convention_only.is_empty());
    }

    #[test]
    fn dead_guard_comment_patterns_mark_audit_config_non_empty() {
        let config = AuditConfig {
            dead_guard_context_comment_patterns: vec!["dual context".to_string()],
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn command_status_contracts_mark_audit_config_non_empty() {
        let config = AuditConfig {
            command_status_contracts: CommandStatusContractConfig {
                required_commands: Vec::new(),
                required_output_error_commands: Vec::new(),
                scenarios: vec![CommandStatusContractScenario {
                    id: "refactor-transform-no-match".to_string(),
                    command: Some("refactor transform".to_string()),
                    file: "tests/fixtures/refactor-transform-no-match.json".to_string(),
                    outcome: None,
                    output_file: false,
                    expected_fields: BTreeMap::from([(
                        "/success".to_string(),
                        serde_json::json!(true),
                    )]),
                    expected_status: None,
                    status_fields: Vec::new(),
                    expected_dry_run: None,
                    dry_run_fields: Vec::new(),
                    expected_success: None,
                    empty_success: false,
                }],
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn remote_execution_safety_config_marks_audit_config_non_empty() {
        let config = AuditConfig {
            remote_execution_safety: RemoteExecutionSafetyConfig {
                path_translation_markers: vec!["rewrite_remote_args".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn test_wiring_config_marks_audit_config_non_empty() {
        let config = AuditConfig {
            test_wiring: TestWiringConfig {
                policies: vec![test_wiring_policy("nested")],
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn detector_profile_marks_audit_config_non_empty_when_customized() {
        let config = AuditConfig {
            detector_profile: DetectorProfileConfig {
                use_builtin_defaults: false,
                version_guard_languages: vec!["rust".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!config.is_empty());
    }

    #[test]
    fn merge_dedupes_core_boundary_leak_config() {
        let mut config = AuditConfig {
            core_boundary_leaks: CoreBoundaryLeakConfig {
                terms: vec!["florpstack".to_string()],
                scan_path_contains: vec!["src/core/".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        config.merge(&AuditConfig {
            dead_guard_context_comment_patterns: vec!["dual context".to_string()],
            core_boundary_leaks: CoreBoundaryLeakConfig {
                terms: vec!["florpstack".to_string(), "widgetlang".to_string()],
                scan_path_contains: vec!["src/core/".to_string(), "src/commands/".to_string()],
                allow_line_contains: vec!["allow-core-boundary-example".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });

        assert_eq!(
            config.core_boundary_leaks.terms,
            vec!["florpstack", "widgetlang"]
        );
        assert_eq!(
            config.core_boundary_leaks.scan_path_contains,
            vec!["src/core/", "src/commands/"]
        );
        assert_eq!(
            config.dead_guard_context_comment_patterns,
            vec!["dual context"]
        );
        assert_eq!(
            config.core_boundary_leaks.allow_line_contains,
            vec!["allow-core-boundary-example"]
        );
    }

    #[test]
    fn merge_dedupes_source_policy_rules_by_id() {
        let mut config = AuditConfig {
            source_policies: vec![source_policy_rule("synthetic-boundary", "florpstack")],
            ..Default::default()
        };

        config.merge(&AuditConfig {
            source_policies: vec![
                source_policy_rule("synthetic-boundary", "widgetlang"),
                source_policy_rule("second-boundary", "gadgetdb"),
            ],
            ..Default::default()
        });

        assert_eq!(config.source_policies.len(), 2);
        assert_eq!(config.source_policies[0].id, "synthetic-boundary");
        assert_eq!(config.source_policies[1].id, "second-boundary");
    }

    fn source_policy_rule(id: &str, term: &str) -> SourcePolicyRule {
        SourcePolicyRule {
            id: id.to_string(),
            kind: "source_policy_violation".to_string(),
            severity: "warning".to_string(),
            convention: "source_policy".to_string(),
            language: None,
            file_extensions: Vec::new(),
            include_path_contains: vec!["src/core/".to_string()],
            exclude_path_contains: Vec::new(),
            allow_line_contains: Vec::new(),
            ignore_line_prefixes: Vec::new(),
            ignore_after_line_equals: Vec::new(),
            example_path_contains: Vec::new(),
            example_classification: None,
            description: "Forbidden term `{term}` at line {line}".to_string(),
            suggestion: "Move the term into component policy.".to_string(),
            rule: SourcePolicyRuleBody::ForbiddenTerms {
                terms: vec![SourcePolicyTerm {
                    value: term.to_string(),
                    label: None,
                    match_mode: None,
                }],
                default_match: SourcePolicyMatchMode::Token,
                case_insensitive: true,
            },
        }
    }

    #[test]
    fn merge_dedupes_remote_execution_safety_config() {
        let mut config = AuditConfig {
            remote_execution_safety: RemoteExecutionSafetyConfig {
                capability_preflight_markers: vec!["capability_plan".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        config.merge(&AuditConfig {
            remote_execution_safety: RemoteExecutionSafetyConfig {
                capability_preflight_markers: vec![
                    "capability_plan".to_string(),
                    "evaluate_remote_capabilities".to_string(),
                ],
                ..Default::default()
            },
            ..Default::default()
        });

        assert_eq!(
            config.remote_execution_safety.capability_preflight_markers,
            vec!["capability_plan", "evaluate_remote_capabilities"]
        );
    }

    #[test]
    fn merge_dedupes_test_wiring_policies_by_id() {
        let mut config = AuditConfig {
            test_wiring: TestWiringConfig {
                policies: vec![test_wiring_policy("nested")],
            },
            ..Default::default()
        };

        config.merge(&AuditConfig {
            test_wiring: TestWiringConfig {
                policies: vec![test_wiring_policy("nested"), test_wiring_policy("external")],
            },
            ..Default::default()
        });

        assert_eq!(config.test_wiring.policies.len(), 2);
        assert_eq!(config.test_wiring.policies[0].id, "nested");
        assert_eq!(config.test_wiring.policies[1].id, "external");
    }

    #[test]
    fn merge_extends_detector_profile_and_preserves_disable_defaults() {
        let mut config = AuditConfig {
            detector_profile: DetectorProfileConfig {
                use_builtin_defaults: false,
                version_guard_languages: vec!["rust".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        config.merge(&AuditConfig {
            detector_profile: DetectorProfileConfig {
                version_guard_languages: vec!["rust".to_string(), "typescript".to_string()],
                version_guard_constants: vec!["RUNTIME_VERSION".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });

        assert!(!config.detector_profile.use_builtin_defaults);
        assert_eq!(
            config.detector_profile.version_guard_languages,
            vec!["rust", "typescript"]
        );
        assert_eq!(
            config.detector_profile.version_guard_constants,
            vec!["RUNTIME_VERSION"]
        );
    }

    fn test_wiring_policy(id: &str) -> TestWiringPolicy {
        TestWiringPolicy {
            id: id.to_string(),
            source_path_globs: vec!["source/**".to_string()],
            test_path_globs: vec!["checks/**".to_string()],
            auto_discovered_test_path_globs: Vec::new(),
            support_test_path_globs: Vec::new(),
            require_explicit_wiring: true,
            explicit_wiring_marker_patterns: vec!["{test_path}".to_string()],
            convention: "test_wiring".to_string(),
            severity: "warning".to_string(),
            description: "`{test_path}` needs wiring".to_string(),
            suggestion: "Add wiring for `{test_path}`".to_string(),
        }
    }
}
