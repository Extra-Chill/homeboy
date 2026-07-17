use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use crate::paths;
pub use homeboy_audit_contract::test_mapping::{
    BehaviorScenarioNames, IncludeWrapperPolicy, PackageNameSource, TestMappingConfig,
    TestVacuityPolicy,
};
use homeboy_audit_contract::AuditConfig;
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

/// Deploy lifecycle: verification rules, install overrides, version patterns, @since tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployCapability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<DeployVerification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<DeployOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_path_suffixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_hints: Vec<DeployOwnerHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archive_install: Vec<DeployArchiveInstallPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_path_inference: Vec<RemotePathInferenceRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_roots: Vec<RemotePathRootRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_patterns: Vec<VersionPatternConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_tag: Option<SinceTagConfig>,
}

/// Test mapping convention: how source files map to test files.
pub use homeboy_extension_contract::test_drift::TestDriftConfig;

/// Docs audit: ignore patterns and feature detection patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCapability {
    /// Shell script that resolves reference dependencies and exports
    /// `HOMEBOY_AUDIT_REFERENCE_PATHS` (newline-separated directory paths).
    /// Reference dependencies are fingerprinted for cross-reference analysis
    /// (dead code detection) but excluded from convention and duplication detection.
    /// Example: framework or package dependencies declared by an extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_references: Option<String>,
    /// Detector rules supplied by this extension for its language/framework.
    #[serde(default, skip_serializing_if = "AuditConfig::is_empty")]
    pub detector_rules: AuditConfig,
    /// Glob patterns for paths to ignore during docs audit.
    /// Uses `*` for single segment and `**` for multiple segments (e.g., `/wp-json/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_claim_patterns: Vec<String>,
    /// Regex patterns to detect feature registrations in source code.
    /// Each pattern should have a capture group for the feature name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feature_patterns: Vec<String>,
    /// Human-readable labels for feature patterns, keyed by a substring of the pattern.
    /// Used by `docs generate --from-audit` to group and label features in generated docs.
    /// Example: `{"register_post_type": "Post Types", "register_rest_route": "REST API Routes"}`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_labels: HashMap<String, String>,
    /// Doc generation targets: maps a feature label to a file path and optional heading.
    /// Used by `docs generate --from-audit` to place features in the right doc files.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub doc_targets: HashMap<String, DocTarget>,
    /// Context extraction rules for feature patterns, keyed by a substring of the pattern.
    /// Tells the audit system what additional context to extract around each detected feature.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_context: HashMap<String, FeatureContextRule>,
    /// Test mapping convention for structural test coverage gap detection.
    /// Defines how source files map to test files and how methods map to test methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_mapping: Option<TestMappingConfig>,
}

/// Executable tool: runtime, inputs, and output schema.
/// Represents a extension that can be run as a standalone tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableCapability {
    pub runtime: RuntimeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputConfig>,

    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Desktop/platform UI config: pinned files, database, discovery, commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

// ============================================================================
// ExtensionManifest
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ExtensionDiagnosticsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ExtensionToolDiagnosticDeclaration>,
}

pub const NOTIFICATION_TRANSPORT_SCHEMA: &str = "homeboy/notification-transport/v1";

/// An installed extension-owned notification command. `command` is a literal
/// argv prefix, never a shell command or template. Homeboy appends the typed
/// completion event arguments defined by the schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationTransportConfig {
    #[serde(default = "default_notification_transport_schema")]
    pub schema: String,
    pub id: String,
    pub command: Vec<String>,
}

fn default_notification_transport_schema() -> String {
    NOTIFICATION_TRANSPORT_SCHEMA.to_string()
}

impl NotificationTransportConfig {
    pub fn validate(&self) -> Result<()> {
        if self.schema != NOTIFICATION_TRANSPORT_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "notification_transports.schema",
                format!("must be {NOTIFICATION_TRANSPORT_SCHEMA}"),
                Some(self.schema.clone()),
                None,
            ));
        }
        let valid_id = !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid_id {
            return Err(Error::validation_invalid_argument(
                "notification_transports.id",
                "must contain 1-128 ASCII letters, digits, '.', '_' or '-'",
                Some(self.id.clone()),
                None,
            ));
        }
        if self.command.is_empty()
            || self
                .command
                .iter()
                .any(|arg| arg.is_empty() || arg.contains('\0'))
        {
            return Err(Error::validation_invalid_argument(
                "notification_transports.command",
                "must be a non-empty literal argv array without empty or NUL values",
                Some(self.id.clone()),
                None,
            ));
        }
        Ok(())
    }
}

impl ExtensionDiagnosticsConfig {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentTaskPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
}

/// Unified extension manifest decomposed into capability groups.
///
/// Extension JSON files use nested capability groups that map directly to these fields.
/// Convenience methods provide ergonomic access to nested data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    // Identity
    #[serde(default, skip_serializing)]
    pub id: String,
    pub name: String,
    pub version: String,

    // What this extension provides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provides: Option<ProvidesConfig>,

    // Capability scripts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ScriptsConfig>,

    // Optional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,

    // Capability groups
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<PlatformCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_env: Option<ComponentEnvConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_provider: Option<EnvProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshotConfig>,

    /// Optional diagnostics this extension wants runner doctor to probe without
    /// core learning the extension's ecosystem or toolchain.
    #[serde(default, skip_serializing_if = "ExtensionDiagnosticsConfig::is_empty")]
    pub diagnostics: ExtensionDiagnosticsConfig,

    /// Versioned, extension-owned completion notification transports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notification_transports: Vec<NotificationTransportConfig>,

    /// Runtime requirements needed to execute this extension's runner scripts.
    /// Component-declared requirements still win; these are fallbacks for the
    /// runner substrate itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeRequirementsConfig>,

    // Standalone capabilities (already self-contained structs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deps: Option<DepsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint: Option<LintConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<TestConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bench: Option<BenchConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuzz: Option<FuzzConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceConfig>,
    /// Post-write verify command used as a safety gate after `refactor --from ...`
    /// autofix writes to disk. If the command exits non-zero, the written files
    /// are reverted and the fixes are reclassified as declined. See #1167.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix_verify: Option<AutofixVerifyConfig>,
    /// Structured run-directory sidecars this extension declares as a public
    /// machine-readable contract.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub structured_sidecars: BTreeMap<String, StructuredSidecarContract>,

    /// Optional runner-resolvable source metadata for materializing this
    /// extension on a runner without depending on controller-local paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization_source: Option<ExtensionMaterializationSourceContract>,

    /// Extension-owned producers Homeboy can invoke at explicit lifecycle times
    /// to obtain generic contracts without learning domain-specific behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contract_producers: Vec<ExtensionContractProducer>,

    /// Release preflights supplied by this extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release_preflights: Vec<ReleasePreflightConfig>,

    /// First-class agent runtime package manifests supplied by this extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_runtimes: Vec<AgentRuntimeManifestConfig>,

    /// Extension-owned agent task policy. Runtime/provider manifests declare
    /// capabilities only; they do not select global defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task: Option<AgentTaskPolicyConfig>,

    // Actions (cross-cutting: used by both platform and executable extensions)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionConfig>,

    // Lifecycle hooks: event name -> list of shell commands.
    // Extension hooks run before component hooks at each event.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<String, Vec<String>>,

    // Shared
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<SettingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequirementsConfig>,

    // Extensibility: preserve unknown fields for external consumers (GUI, workflows)
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,

    // Internal path (not serialized)
    #[serde(skip)]
    pub extension_path: Option<String>,
}

impl ExtensionManifest {
    pub fn validate_notification_transports(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for transport in &self.notification_transports {
            transport.validate()?;
            if !ids.insert(&transport.id) {
                return Err(Error::validation_invalid_argument(
                    "notification_transports.id",
                    "must be unique within an extension manifest",
                    Some(transport.id.clone()),
                    None,
                ));
            }
        }
        Ok(())
    }
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
    }

    /// Setting keys this extension declares it understands.
    ///
    /// Used to validate `--setting` / `--setting-json` overrides before a
    /// run: a key the extension never consumes (a typo like `bench_env`
    /// vs `workflow_bench_env`) silently does nothing today and can waste
    /// a long proof run. Returns the declared `id`s from the manifest's
    /// `settings` block. An empty result means the extension declares no
    /// settings — callers should treat that as "cannot validate" rather
    /// than "rejects everything".
    pub fn accepted_setting_keys(&self) -> Vec<String> {
        self.settings
            .iter()
            .map(|setting| setting.id.clone())
            .collect()
    }

    pub fn has_build(&self) -> bool {
        self.build.is_some()
    }

    pub fn has_lint(&self) -> bool {
        self.lint
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn has_deps(&self) -> bool {
        self.deps
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn has_test(&self) -> bool {
        self.test
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn has_bench(&self) -> bool {
        self.bench
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn has_fuzz(&self) -> bool {
        self.fuzz.is_some()
    }

    pub fn has_trace(&self) -> bool {
        self.trace
            .as_ref()
            .and_then(|c| c.extension_script.as_ref())
            .is_some()
    }

    pub fn lint_script(&self) -> Option<&str> {
        self.lint
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn build_script(&self) -> Option<&str> {
        self.build
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn deps_script(&self) -> Option<&str> {
        self.deps
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn test_script(&self) -> Option<&str> {
        self.test
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn bench_script(&self) -> Option<&str> {
        self.bench
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn fuzz_script(&self) -> Option<&str> {
        self.fuzz
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn fuzz_workloads(&self) -> &[FuzzWorkloadConfig] {
        self.fuzz
            .as_ref()
            .map(|fuzz| fuzz.workloads.as_slice())
            .unwrap_or(&[])
    }

    pub fn trace_script(&self) -> Option<&str> {
        self.trace
            .as_ref()
            .and_then(|c| c.extension_script.as_deref())
    }

    pub fn trace_runner_capabilities(&self) -> &[String] {
        self.trace
            .as_ref()
            .map(|trace| trace.runner_capabilities.as_slice())
            .unwrap_or(&[])
    }

    pub fn trace_toolchain_provenance(
        &self,
    ) -> &[super::manifest_config::TraceToolchainProvenanceConfig] {
        self.trace
            .as_ref()
            .map(|trace| trace.toolchain_provenance.as_slice())
            .unwrap_or(&[])
    }

    pub fn trace_browser_evidence(
        &self,
    ) -> &[super::manifest_config::TraceBrowserEvidenceAdapterConfig] {
        self.trace
            .as_ref()
            .map(|trace| trace.browser_evidence.as_slice())
            .unwrap_or(&[])
    }

    pub fn env_provider_script(&self) -> Option<&str> {
        self.env_provider
            .as_ref()
            .map(|provider| provider.script.as_str())
    }

    /// Convenience accessor for the optional test mapping config
    /// declared under the audit capability.
    pub fn test_mapping(&self) -> Option<&TestMappingConfig> {
        self.audit.as_ref().and_then(|a| a.test_mapping.as_ref())
    }

    /// Convenience accessor for the test drift selection contract.
    ///
    /// Only the canonical `test.drift` field declares drift behavior.
    pub fn test_drift(&self) -> Option<TestDriftConfig> {
        self.test.as_ref().and_then(|t| t.drift.clone())
    }

    /// Convenience accessor for extension-supplied generic audit detector rules.
    pub fn audit_detector_rules(&self) -> Option<&AuditConfig> {
        self.audit.as_ref().map(|a| &a.detector_rules)
    }

    /// Convenience: autofix verify config, if this extension declares one.
    /// See [`AutofixVerifyConfig`] for the contract.
    pub fn autofix_verify(&self) -> Option<&AutofixVerifyConfig> {
        self.autofix_verify.as_ref()
    }

    /// Structured sidecars this extension explicitly declares.
    /// Missing declarations mean the extension has no structured sidecar
    /// contract for that output.
    pub fn structured_sidecars(&self) -> Vec<StructuredSidecarDeclaration> {
        self.structured_sidecars
            .iter()
            .filter_map(|(name, contract)| contract.declaration(name))
            .collect()
    }

    /// Schema version declared by the canonical `structured_sidecars` manifest
    /// section for a logical sidecar name.
    pub fn structured_sidecar_schema_version(&self, name: &str) -> Option<&str> {
        self.structured_sidecars
            .get(name)
            .and_then(|contract| match contract {
                StructuredSidecarContract::Enabled(true) => {
                    crate::structured_sidecar::default_schema_version(name)
                }
                StructuredSidecarContract::Enabled(false) => None,
                StructuredSidecarContract::Detail(detail) if detail.enabled => detail
                    .schema_version
                    .as_deref()
                    .or_else(|| crate::structured_sidecar::default_schema_version(name)),
                StructuredSidecarContract::Detail(_) => None,
            })
    }

    /// Convenience: get deploy verifications (empty if no deploy capability).
    pub fn deploy_verifications(&self) -> &[DeployVerification] {
        self.deploy
            .as_ref()
            .map(|d| d.verifications.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get deploy overrides (empty if no deploy capability).
    pub fn deploy_overrides(&self) -> &[DeployOverride] {
        self.deploy
            .as_ref()
            .map(|d| d.overrides.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get archive-install deploy policies (empty if no deploy capability).
    pub fn deploy_archive_installs(&self) -> &[DeployArchiveInstallPolicy] {
        self.deploy
            .as_ref()
            .map(|d| d.archive_install.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get remote path inference rules (empty if no deploy capability).
    pub fn remote_path_inference_rules(&self) -> &[RemotePathInferenceRule] {
        self.deploy
            .as_ref()
            .map(|d| d.remote_path_inference.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get version patterns (empty if no deploy capability).
    pub fn version_patterns(&self) -> &[VersionPatternConfig] {
        self.deploy
            .as_ref()
            .map(|d| d.version_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get since_tag config.
    pub fn since_tag(&self) -> Option<&SinceTagConfig> {
        self.deploy.as_ref().and_then(|d| d.since_tag.as_ref())
    }

    /// Convenience: get runtime config.
    pub fn runtime(&self) -> Option<&RuntimeConfig> {
        self.executable.as_ref().map(|e| &e.runtime)
    }

    /// Convenience: get inputs (empty if no executable capability).
    pub fn inputs(&self) -> &[InputConfig] {
        self.executable
            .as_ref()
            .map(|e| e.inputs.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get audit reference setup script path (relative to extension dir).
    pub fn audit_setup_references(&self) -> Option<&str> {
        self.audit
            .as_ref()
            .and_then(|a| a.setup_references.as_deref())
    }

    /// Convenience: get audit ignore claim patterns (empty if no audit capability).
    pub fn audit_ignore_claim_patterns(&self) -> &[String] {
        self.audit
            .as_ref()
            .map(|a| a.ignore_claim_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get audit feature patterns (empty if no audit capability).
    pub fn audit_feature_patterns(&self) -> &[String] {
        self.audit
            .as_ref()
            .map(|a| a.feature_patterns.as_slice())
            .unwrap_or(&[])
    }

    /// Convenience: get feature labels map (empty if no audit capability).
    pub fn audit_feature_labels(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.feature_labels)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get doc targets map (empty if no audit capability).
    pub fn audit_doc_targets(&self) -> &HashMap<String, DocTarget> {
        static EMPTY: std::sync::LazyLock<HashMap<String, DocTarget>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.doc_targets)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get feature context rules (empty if no audit capability).
    pub fn audit_feature_context(&self) -> &HashMap<String, FeatureContextRule> {
        static EMPTY: std::sync::LazyLock<HashMap<String, FeatureContextRule>> =
            std::sync::LazyLock::new(HashMap::new);
        self.audit
            .as_ref()
            .map(|a| &a.feature_context)
            .unwrap_or(&EMPTY)
    }

    /// Convenience: get database config from platform capability.
    pub fn database(&self) -> Option<&DatabaseConfig> {
        self.platform.as_ref().and_then(|p| p.database.as_ref())
    }

    /// Parse the version string as semver.
    pub fn semver(&self) -> crate::error::Result<semver::Version> {
        super::version::parse_extension_version(&self.version, &self.id)
    }

    /// Get file extensions this extension provides (empty if not specified).
    pub fn provided_file_extensions(&self) -> &[String] {
        self.provides
            .as_ref()
            .map(|p| p.file_extensions.as_slice())
            .unwrap_or(&[])
    }

    /// Get capabilities this extension provides (empty if not specified).
    pub fn provided_capabilities(&self) -> &[String] {
        self.provides
            .as_ref()
            .map(|p| p.capabilities.as_slice())
            .unwrap_or(&[])
    }

    /// Get component discovery marker rules (empty if not specified).
    pub fn discovery_markers(&self) -> &[DiscoveryMarkerConfig] {
        self.provides
            .as_ref()
            .map(|p| p.discovery_markers.as_slice())
            .unwrap_or(&[])
    }

    /// Check if this extension handles a given file extension.
    pub fn handles_file_extension(&self, ext: &str) -> bool {
        self.provided_file_extensions().iter().any(|e| e == ext)
    }

    /// Get the fingerprint script path (relative to extension dir), if configured.
    pub fn fingerprint_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.fingerprint.as_deref())
    }

    /// Get the refactor script path (relative to extension dir), if configured.
    pub fn refactor_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.refactor.as_deref())
    }

    /// Get the topology script path (relative to extension dir), if configured.
    pub fn topology_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.topology.as_deref())
    }

    /// Get the validate script path (relative to extension dir), if configured.
    pub fn validate_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.validate.as_deref())
    }

    /// Get the format script path (relative to extension dir), if configured.
    pub fn format_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.format.as_deref())
    }

    /// Get the compiler warning script path (relative to extension dir), if configured.
    pub fn compiler_warnings_script(&self) -> Option<&str> {
        self.scripts
            .as_ref()
            .and_then(|s| s.compiler_warnings.as_deref())
    }

    /// Get the compiler warning fixes script path (relative to extension dir), if configured.
    pub fn compiler_warning_fixes_script(&self) -> Option<&str> {
        self.scripts
            .as_ref()
            .and_then(|s| s.compiler_warning_fixes.as_deref())
    }

    /// Get the contract script path (relative to extension dir), if configured.
    pub fn contract_script(&self) -> Option<&str> {
        self.scripts.as_ref().and_then(|s| s.contract.as_deref())
    }
}

impl ConfigEntity for ExtensionManifest {
    const ENTITY_TYPE: &'static str = "extension";
    const DIR_NAME: &'static str = "extensions";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::extension_not_found(id, suggestions)
    }

    /// Override: extensions use `{dir}/{id}/{id}.json` pattern.
    fn config_path(id: &str) -> Result<PathBuf> {
        paths::extension_manifest(id)
    }
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
