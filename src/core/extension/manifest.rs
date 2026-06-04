use crate::core::component::AuditConfig;
use crate::core::config::ConfigEntity;
use crate::core::error::{Error, Result};
use crate::core::paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

// Keep broad manifest examples on this baselined module while leaf config
// structs live in focused files: PHP extensions, cargo checks, phpcs/phpstan steps.
pub use super::manifest_action_config::{
    ActionConfig, InputConfig, OutputConfig, OutputSchema, RuntimeConfig, SelectOption,
    SettingConfig,
};
pub use super::manifest_config::{
    AutofixVerifyConfig, BenchConfig, BuildConfig, CliAutoFlag, CliAutoFlagCondition, CliConfig,
    CliHelpConfig, DatabaseCliConfig, DatabaseConfig, DeployOverride, DeployOwnerHint,
    DeployVerification, DepsConfig, DiscoveryConfig, EnvProviderConfig, FileContainsCondition,
    LintChangedFileRoute, LintConfig, RemotePathInferenceRule, RemotePathRootRule,
    RequirementsConfig, SinceTagConfig, TestChangedFileExclusiveEnv, TestChangedFileRouting,
    TestChangedFileRoutingStrategy, TestConfig, TraceConfig, VersionPatternConfig,
};
pub use super::manifest_deploy_config::DeployArchiveInstallPolicy;
pub use super::manifest_sidecar::{StructuredSidecarContract, StructuredSidecarDeclaration};

/// Type of action that can be executed by a extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionType {
    Api,
    Command,
    Builtin,
}

/// Builtin action types for Desktop app (copy, export operations).
/// CLI parses these but does not execute them - Desktop implements the behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinAction {
    CopyColumn,
    ExportCsv,
    CopyJson,
}

/// HTTP method for API actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

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
/// Used by the audit pipeline for structural test coverage gap detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestMappingConfig {
    /// Source directories to scan (relative to component root).
    /// Example: `["src"]` for Rust, `["inc"]` for WordPress.
    pub source_dirs: Vec<String>,
    /// Test directories to scan (relative to component root).
    /// Example: `["tests"]` for both Rust and WordPress.
    pub test_dirs: Vec<String>,
    /// How source file paths map to test file paths.
    /// Template variables: `{dir}` (relative dir), `{name}` (filename without ext), `{ext}` (extension).
    /// Example Rust: `"tests/{dir}/{name}_test.{ext}"` or inline `#[cfg(test)]`
    /// Example WordPress: `"tests/Unit/{dir}/{name}Test.{ext}"`
    pub test_file_pattern: String,
    /// Prefix for test method names (e.g., `"test_"` for both Rust and PHP).
    #[serde(default = "default_test_prefix")]
    pub method_prefix: String,
    /// Whether the language uses inline tests (e.g., Rust `#[cfg(test)]` in the same file).
    #[serde(default)]
    pub inline_tests: bool,
    /// Directory path patterns that indicate high-priority test coverage.
    /// Files in matching directories get `Warning` severity instead of `Info`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_patterns: Vec<String>,
    /// Path patterns to exclude from test coverage checks entirely.
    /// Files matching any pattern are skipped for both missing_test_file and
    /// missing_test_method findings. Use for CLI wrappers, pure type definitions,
    /// and other structurally untestable code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_test_patterns: Vec<String>,
}

/// Test drift convention: how source and test files are selected for drift scans.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestDriftConfig {
    /// Source directories to scan (relative to component root).
    pub source_dirs: Vec<String>,
    /// Test directories to scan (relative to component root).
    pub test_dirs: Vec<String>,
    /// File extensions to include when building source/test glob patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Whether the language supports inline tests. Stored for consumers that
    /// need it; drift scanning still uses source/test glob patterns.
    #[serde(default)]
    pub inline_tests: bool,
}

fn default_test_prefix() -> String {
    "test_".to_string()
}

/// Docs audit: ignore patterns and feature detection patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCapability {
    /// Shell script that resolves reference dependencies and exports
    /// `HOMEBOY_AUDIT_REFERENCE_PATHS` (newline-separated directory paths).
    /// Reference dependencies are fingerprinted for cross-reference analysis
    /// (dead code detection) but excluded from convention and duplication detection.
    /// Example: WordPress core + plugin dependencies.
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

/// Rules for extracting context around a detected feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureContextRule {
    /// Extract doc comments above the feature (///, /**, #, etc.).
    #[serde(default)]
    pub doc_comment: bool,
    /// Extract fields/items from the block following the feature (struct fields, enum variants).
    #[serde(default)]
    pub block_fields: bool,
}

/// Where a feature category should be rendered in documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocTarget {
    /// Relative path within the docs directory (e.g., "api-reference.md").
    pub file: String,
    /// Heading under which features are listed (e.g., "## Endpoints").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// Template for rendering each feature. Uses `{name}`, `{source_file}`, `{line}`.
    /// Default: `- \`{name}\` ({source_file}:{line})`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Executable tool: runtime, inputs, and output schema.
/// Represents a extension that can be run as a standalone tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableCapability {
    pub runtime: RuntimeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,
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

/// Component environment detection supplied by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEnvConfig {
    /// Script path relative to the extension directory.
    /// Runs from the component root and emits JSON such as
    /// {"runtimes":{"php":{"version":"8.1"}}}.
    pub detect_script: String,
}

/// CI reproduction profiles declared by an extension.
///
/// Core treats these as explicit local-reproduction contracts. Best-effort
/// workflow discovery can inventory CI files, but runnable equivalence comes
/// from these declarations.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiCapability {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, CiProfileSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub jobs: BTreeMap<String, CiJobSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiProfileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum CiJobFidelity {
    LocalEquivalent,
    LocalPartial,
    RemoteOnly,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiJobSpec {
    #[serde(flatten)]
    pub mapping: CiJobMapping,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiJobMapping {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiLocalContext {
    #[serde(default)]
    pub fidelity: CiJobFidelity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

// ============================================================================
// ExtensionManifest
// ============================================================================

/// What a extension provides: file extensions it handles and capabilities it supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidesConfig {
    /// File extensions this extension can process (e.g., ["php", "inc"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Capabilities this extension supports (e.g., ["fingerprint", "refactor"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Component-root marker rules used to suggest this extension for an
    /// unattached component. Core evaluates these generically; extension
    /// manifests own the ecosystem-specific file/glob knowledge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovery_markers: Vec<DiscoveryMarkerConfig>,
}

/// Component-root marker rule for extension discovery suggestions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoveryMarkerConfig {
    /// Marker paths/globs that must all match relative to the component root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<String>,
    /// Marker paths/globs where any single match is sufficient.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<String>,
}

/// Scripts that implement extension capabilities.
/// Each key maps a capability name to a script path relative to the extension directory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptsConfig {
    /// Script that extracts structural fingerprints from source files.
    /// Receives file content on stdin, outputs FileFingerprint JSON on stdout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Script that applies refactoring edits to source files.
    /// Receives edit instructions on stdin, outputs transformed content on stdout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refactor: Option<String>,
    /// Script that classifies files/artifacts for test topology auditing.
    /// Receives `{file_path, content}` on stdin and outputs `{artifacts:[...]}` on stdout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<String>,
    /// Script that validates written code compiles/parses correctly.
    /// Receives `{root, changed_files}` JSON on stdin, exits 0 on success, non-zero with
    /// compiler output on stderr on failure.
    ///
    /// Language examples:
    /// - Rust: `cargo check`
    /// - PHP: `php -l` on each changed file
    /// - TypeScript: `tsc --noEmit`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validate: Option<String>,
    /// Script that formats source code after automated writes.
    /// Runs from the project root. Exit 0 on success, non-zero on failure.
    /// Formatting failure is non-fatal — it logs a warning but never rolls back.
    ///
    /// Language examples:
    /// - Rust: `cargo fmt`
    /// - TypeScript: `npx prettier --write .`
    /// - PHP: `vendor/bin/phpcbf`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Script that collects compiler warnings.
    /// Runs from the project root and receives `{root}` JSON on stdin.
    /// Outputs `{warnings:[...]}` JSON using Homeboy's generic warning envelope.
    /// Split lint runners may use step selectors such as `phpcs,phpstan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_warnings: Option<String>,
    /// Script that converts compiler warnings into machine-applicable fixes.
    /// Runs from the project root and receives `{root, findings}` JSON on stdin.
    /// Outputs `{fixes:[...]}` JSON using Homeboy's generic fix envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_warning_fixes: Option<String>,
    /// Script that extracts function contracts from source files.
    /// Receives `{file, content}` JSON on stdin, outputs `{file, contracts: [...]}` JSON on stdout.
    /// Each contract describes a function's signature, control flow branches, effects, and calls.
    ///
    /// Used by the test generator, doc generator, and refactor safety checker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
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
    pub fn has_cli(&self) -> bool {
        self.cli.is_some()
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
                StructuredSidecarContract::Detail(detail) if detail.enabled => {
                    detail.schema_version.as_deref()
                }
                _ => None,
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
    pub fn semver(&self) -> crate::core::error::Result<semver::Version> {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequirementsConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub runtimes: HashMap<String, RuntimeRequirementConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequirementConfig {
    pub version: String,
}
