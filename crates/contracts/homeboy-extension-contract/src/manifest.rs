//! The extension manifest data model.
//!
//! `ExtensionManifest` and its pure accessors live here as behavior-free data.
//! The `ConfigEntity` trait impl and the two run-dir-dependent sidecar helpers
//! (`structured_sidecars`, `structured_sidecar_schema_version`) stay in
//! `homeboy-core`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::autofix_config::*;
use crate::ci_config::*;
use crate::extension_contract_producer::*;
use crate::external_check_detail_resolver::*;
use crate::external_storage_retention::*;
use crate::fuzz_config::*;
use crate::manifest_action_config::*;
use crate::manifest_artifact_cleanup::*;
use crate::manifest_capabilities::*;
use crate::manifest_capability_config::*;
use crate::manifest_deploy_config::*;
use crate::manifest_toolchain_config::*;
use crate::notification_transport_config::*;
use crate::sidecar_config::*;
use crate::test_drift::*;
use crate::trace_config::*;
use homeboy_audit_contract::*;

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
    /// Recipe-run providers this extension publishes.
    ///
    /// Typed since #13724. Malformed entries are retained rather than rejected
    /// so `runner recipe-providers` can name the broken declaration instead of
    /// reporting the whole manifest as unreadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipe_run_providers: Vec<RecipeRunProviderDeclaration>,
    /// Deployment providers this extension publishes.
    ///
    /// Typed since #13723. While this rode in `extra`, a malformed descriptor
    /// was `.ok()`-discarded and surfaced as "extension declares no providers",
    /// which is indistinguishable from a correct manifest that declares none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deployment_providers: Vec<DeploymentProviderManifest>,
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

    /// Extension-owned, opt-in hydration for failed external CI statuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_check_detail_resolvers: Vec<ExternalCheckDetailResolverConfig>,

    /// Runtime requirements needed to execute this extension's runner scripts.
    /// Component-declared requirements still win; these are fallbacks for the
    /// runner substrate itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeRequirementsConfig>,

    /// Extension-owned executable/toolchain probes for Lab admission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolchain_readiness: Vec<ToolchainReadinessProbe>,

    // Standalone capabilities (already self-contained structs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,
    /// Reconstructable install/build trees this extension owns, plus the
    /// rehydration guidance for them. Core resolves and gates these; the
    /// extension only declares what is reconstructable.
    #[serde(default, skip_serializing_if = "ArtifactCleanupConfig::is_empty")]
    pub artifact_cleanup: ArtifactCleanupConfig,
    /// Provider-owned external runtime storage. Unlike `artifact_cleanup`,
    /// providers discover paths outside checkouts and perform native reclaim.
    #[serde(
        default,
        skip_serializing_if = "ExternalStorageRetentionConfig::is_empty"
    )]
    pub external_storage_retention: ExternalStorageRetentionConfig,
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

    // Multi-extension composition: `includes` primacy is used to disambiguate a
    // capability provided by more than one linked extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<CompositionConfig>,

    /// Unknown top-level manifest keys, preserved rather than rejected so an
    /// extension published against a newer or older core still deserializes.
    ///
    /// This is a *forward-compatibility buffer*, not an extension point. Core
    /// reads exactly one key out of it — the legacy camelCase `sourceUrl`
    /// (`lifecycle::source_metadata`) — and nothing else in here has a reader.
    /// `deployment_providers` (#13723) and `recipe_run_providers` (#13724) were
    /// the other two and are now typed fields.
    ///
    /// With only a legacy alias left, a key appearing here is a key nothing
    /// will ever act on.
    ///
    /// Landing anything else here makes it inert *silently*, which is how
    /// shipped manifests accumulated `required_output_declarations` (26 lines),
    /// `testing`, `docs`, `component_type`, and `homeboy_version_target`: all
    /// parsed, all survived a round-trip, none were ever consulted by anything
    /// in this workspace. Extra-Chill/homeboy-extensions#2565 removed that set
    /// at the source.
    ///
    /// Two things deliberately stayed, and the distinction matters: no reader
    /// *here* is not the same as no reader *anywhere*.
    /// `dependency_materialization_recipes` (51 lines) is still inert to core
    /// but is asserted on by the WordPress extension's own smoke tests.
    /// managed-preview's `browser_trace_helpers` and `preview_backends` have no
    /// reader in this workspace or in the extensions repo, which makes their
    /// inertness an unprovable negative from here rather than a demonstrated
    /// one — so they were left alone.
    ///
    /// A key that core is meant to act on belongs on a typed field, where its
    /// absence of a reader is a compile-time fact rather than something a grep
    /// has to discover. (#11124)
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,

    // Internal path (not serialized)
    #[serde(skip)]
    pub extension_path: Option<String>,
}

/// Multi-extension composition metadata.
///
/// `includes` is the sole ownership signal: when several linked extensions
/// provide the same capability, an extension that composes all the others is
/// resolved as the primary owner. See
/// `homeboy_core::extension_execution::disambiguate_capability_owner`.
///
/// Unknown keys are ignored rather than rejected, so manifests still carrying
/// the retired `roles`/`optional`/`conflicts` metadata continue to load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionConfig {
    /// Extensions this one composes with. Used to resolve capability ownership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
}

impl ExtensionManifest {
    pub fn validate_notification_transports(&self) -> homeboy_error::Result<()> {
        if let Some(test) = &self.test {
            test.portable_env.validate()?;
            crate::manifest_toolchain_config::validate_test_secret_env_references(
                &test.secret_env,
            )?;
            crate::manifest_toolchain_config::validate_test_secret_env_projections(
                &test.secret_env_projections,
            )?;
        }
        let mut ids = std::collections::HashSet::new();
        for transport in &self.notification_transports {
            transport.validate()?;
            if !ids.insert(&transport.id) {
                return Err(homeboy_error::Error::validation_invalid_argument(
                    "notification_transports.id",
                    "must be unique within an extension manifest",
                    Some(transport.id.clone()),
                    None,
                ));
            }
        }
        let mut providers = std::collections::HashSet::new();
        for resolver in &self.external_check_detail_resolvers {
            resolver.validate()?;
            if !providers.insert(&resolver.provider) {
                return Err(homeboy_error::Error::validation_invalid_argument(
                    "external_check_detail_resolvers.provider",
                    "must be unique within an extension manifest",
                    Some(resolver.provider.clone()),
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

    /// Whether this extension contributes audit reference paths.
    ///
    /// An `audit` block that only carries detector rules, ignore patterns, or
    /// doc targets is not an executable audit capability — only
    /// `setup_references` is.
    pub fn has_audit(&self) -> bool {
        self.audit
            .as_ref()
            .and_then(|c| c.setup_references.as_ref())
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

    pub fn audit_script(&self) -> Option<&str> {
        self.audit
            .as_ref()
            .and_then(|c| c.setup_references.as_deref())
    }

    pub fn trace_runner_capabilities(&self) -> &[String] {
        self.trace
            .as_ref()
            .map(|trace| trace.runner_capabilities.as_slice())
            .unwrap_or(&[])
    }

    pub fn trace_toolchain_provenance(
        &self,
    ) -> &[crate::trace_config::TraceToolchainProvenanceConfig] {
        self.trace
            .as_ref()
            .map(|trace| trace.toolchain_provenance.as_slice())
            .unwrap_or(&[])
    }

    pub fn trace_browser_evidence(
        &self,
    ) -> &[crate::trace_config::TraceBrowserEvidenceAdapterConfig] {
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
    pub fn semver(&self) -> homeboy_error::Result<semver::Version> {
        crate::version::parse_extension_version(&self.version, &self.id)
    }

    /// Get file extensions this extension provides (empty if not specified).
    pub fn provided_file_extensions(&self) -> &[String] {
        self.provides
            .as_ref()
            .map(|p| p.file_extensions.as_slice())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(transports: Vec<NotificationTransportConfig>) -> ExtensionManifest {
        ExtensionManifest {
            id: "test".to_string(),
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            notification_transports: transports,
            ..serde_json::from_value(serde_json::json!({
                "name": "Test",
                "version": "1.0.0"
            }))
            .expect("minimal manifest")
        }
    }

    fn transport(id: &str) -> NotificationTransportConfig {
        NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: id.to_string(),
            command: vec!["notify".to_string()],
            route_resolver: None,
        }
    }

    #[test]
    fn notification_transport_validation_allows_zero_and_one_declarations() {
        manifest(Vec::new())
            .validate_notification_transports()
            .expect("zero declarations are valid");
        manifest(vec![transport("example.completed")])
            .validate_notification_transports()
            .expect("one declaration is valid");
    }

    #[test]
    fn notification_transport_validation_rejects_duplicate_ids() {
        let error = manifest(vec![
            transport("example.completed"),
            transport("example.completed"),
        ])
        .validate_notification_transports()
        .expect_err("duplicate declarations must fail");

        assert!(error.to_string().contains("notification_transports.id"));
        assert!(error.to_string().contains("unique"));
    }
}

#[cfg(test)]
mod composition_tests {
    use super::*;

    #[test]
    fn deserializes_includes() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "wordpress",
            "version": "1.0.0",
            "composition": { "includes": ["nodejs"] }
        }))
        .expect("manifest with composition deserializes");

        let composition = manifest.composition.expect("composition present");
        assert_eq!(composition.includes, vec!["nodejs".to_string()]);
    }

    /// Manifests published before `roles`/`optional`/`conflicts` were retired
    /// must still load — the keys are simply ignored rather than rejected.
    #[test]
    fn retired_composition_keys_are_tolerated() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "wordpress",
            "version": "1.0.0",
            "composition": {
                "includes": ["nodejs"],
                "optional": ["dependency-adapters/nodejs-package-managers"],
                "roles": {
                    "javascript": "nodejs",
                    "project": ["wordpress-plugin", "wordpress-theme"]
                },
                "conflicts": []
            }
        }))
        .expect("manifest with retired composition keys still deserializes");

        let composition = manifest.composition.expect("composition present");
        assert_eq!(composition.includes, vec!["nodejs".to_string()]);
    }

    #[test]
    fn composition_is_optional() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "nodejs",
            "version": "1.0.0"
        }))
        .expect("manifest without composition deserializes");
        assert!(manifest.composition.is_none());
    }
}
