pub use homeboy_audit_contract::test_mapping::{
    BehaviorScenarioNames, IncludeWrapperPolicy, PackageNameSource, TestMappingConfig,
    TestVacuityPolicy,
};
use homeboy_audit_contract::AuditConfig;
use homeboy_core::config::ConfigEntity;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;
pub use homeboy_extension_contract::extension_contract_producer::{
    ExtensionContractProducer, ExtensionContractProducerInvocation,
    ExtensionContractProducerOutput, ExtensionContractProducerOutputKind,
    ExtensionContractProducerPhase, ExtensionMaterializationHelperManifestRef,
    ExtensionMaterializationSourceContract, ExtensionMaterializationSourceKind,
    EXTENSION_CONTRACT_PRODUCER_SCHEMA, EXTENSION_MATERIALIZATION_SOURCE_SCHEMA,
};
pub use homeboy_extension_contract::manifest_capability_config::{
    AgentRuntimeManifestConfig, ComponentEnvConfig, DiscoveryMarkerConfig, DocTarget,
    ExtensionToolDiagnosticDeclaration, FeatureContextRule, ProvidesConfig, ReleasePreflightConfig,
    RuntimeRequirementsConfig, ScriptsConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

// Keep broad manifest wiring here while leaf config structs live in focused files.
pub use super::manifest_config::{
    AutofixVerifyConfig, TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserMetricAliasConfig, TraceBrowserSummaryAliasConfig, TraceConfig,
};
pub use super::manifest_sidecar::{StructuredSidecarContract, StructuredSidecarDeclaration};
pub use homeboy_extension_contract::ci_config::{
    CiCapability, CiJobFidelity, CiJobMapping, CiJobSpec, CiLocalContext, CiProfileSpec,
};
pub use homeboy_extension_contract::fuzz_config::{FuzzConfig, FuzzWorkloadConfig};
pub use homeboy_extension_contract::manifest_action_config::{
    ActionConfig, InputConfig, RuntimeConfig, SelectOption, SettingConfig,
};
pub use homeboy_extension_contract::manifest_capabilities::{
    AgentTaskPolicyConfig, AuditCapability, DeployCapability, ExecutableCapability,
    PlatformCapability,
};
pub use homeboy_extension_contract::manifest_toolchain_config::{
    BenchConfig, BuildConfig, CliAutoFlag, CliAutoFlagCondition, CliConfig, CliHelpConfig,
    DatabaseCliConfig, DatabaseConfig, DeployOverride, DeployOwnerHint, DeployVerification,
    DepsConfig, DiscoveryConfig, EnvProviderConfig, FileContainsCondition, LintChangedFileRoute,
    LintConfig, RemotePathInferenceRule, RemotePathRootRule, RequirementsConfig, SinceTagConfig,
    SourceSnapshotConfig, TestChangedFileExclusiveEnv, TestChangedFileRouting,
    TestChangedFileRoutingStrategy, TestConfig, VersionPatternConfig,
};
pub use homeboy_extension_contract::DeployArchiveInstallPolicy;
pub use homeboy_extension_contract::{TestPassthroughFilter, TestPassthroughFilterStrategy};

pub use homeboy_extension_contract::action_types::{ActionType, BuiltinAction, HttpMethod};

// ============================================================================
// Capability Groups
// ============================================================================

/// Test mapping convention: how source files map to test files.
pub use homeboy_extension_contract::test_drift::TestDriftConfig;

// ============================================================================
// ExtensionManifest
// ============================================================================

pub use homeboy_extension_contract::manifest_capability_config::ExtensionDiagnosticsConfig;
pub use homeboy_extension_contract::notification_transport_config::{
    NotificationTransportConfig, NOTIFICATION_TRANSPORT_SCHEMA,
};

pub use homeboy_extension_contract::ExtensionManifest;

// Sidecar-declaration helpers depend on core run-dir constants, so they stay
// in core as free functions rather than moving with the manifest data model.
/// Structured sidecars this extension explicitly declares.
/// Missing declarations mean the extension has no structured sidecar
/// contract for that output.
pub fn structured_sidecars(manifest: &ExtensionManifest) -> Vec<StructuredSidecarDeclaration> {
    manifest
        .structured_sidecars
        .iter()
        .filter_map(|(name, contract)| {
            super::manifest_sidecar::structured_sidecar_declaration(contract, name)
        })
        .collect()
}

/// Schema version declared by the canonical `structured_sidecars` manifest
/// section for a logical sidecar name.
pub fn structured_sidecar_schema_version<'a>(
    manifest: &'a ExtensionManifest,
    name: &'a str,
) -> Option<&'a str> {
    manifest
        .structured_sidecars
        .get(name)
        .and_then(|contract| match contract {
            StructuredSidecarContract::Enabled(true) => {
                homeboy_core::structured_sidecar::default_schema_version(name)
            }
            StructuredSidecarContract::Enabled(false) => None,
            StructuredSidecarContract::Detail(detail) if detail.enabled => detail
                .schema_version
                .as_deref()
                .or_else(|| homeboy_core::structured_sidecar::default_schema_version(name)),
            StructuredSidecarContract::Detail(_) => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_transport_requires_versioned_literal_argv_contract() {
        let invalid = NotificationTransportConfig {
            schema: "wrong".to_string(),
            id: "test.run-completion".to_string(),
            command: vec!["true".to_string()],
        };
        assert!(invalid.validate().is_err());
        let invalid = NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: "bad id".to_string(),
            command: vec!["true".to_string()],
        };
        assert!(invalid.validate().is_err());
        let invalid = NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: "test.run-completion".to_string(),
            command: vec![],
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn effective_trivial_method_names_falls_back_to_builtin_set() {
        // No config-declared idiomatic names → core uses the builtin agnostic
        // set so existing behavior is preserved without the detector embedding
        // the literals.
        let config = TestMappingConfig::default();
        let names = config.effective_trivial_method_names();
        assert!(names.iter().any(|n| n == "len"));
        assert!(names.iter().any(|n| n == "__construct"));

        let prefixes = config.effective_trivial_method_prefixes();
        assert!(prefixes.iter().any(|p| p == "get_"));
        assert!(prefixes.iter().any(|p| p == "is_"));
    }

    #[test]
    fn effective_trivial_method_names_honors_configured_policy() {
        // A project/extension-declared policy fully replaces the builtin set —
        // language/ecosystem conventions live in config, not in core.
        let config = TestMappingConfig {
            trivial_method_names: vec!["only_this".to_string()],
            trivial_method_prefixes: vec!["fetch_".to_string()],
            ..Default::default()
        };

        let names = config.effective_trivial_method_names();
        assert_eq!(names, vec!["only_this".to_string()]);
        // Builtin literals are not silently merged in.
        assert!(!names.iter().any(|n| n == "len"));

        let prefixes = config.effective_trivial_method_prefixes();
        assert_eq!(prefixes, vec!["fetch_".to_string()]);
        assert!(!prefixes.iter().any(|p| p == "get_"));
    }
}
