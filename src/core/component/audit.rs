use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "../../../tests/core/component/audit_test.rs"]
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectorProfileConfig {
    /// Include Homeboy's built-in detector profile defaults. Components can set
    /// this to false when they provide a fully custom, non-PHP/non-WordPress profile.
    #[serde(
        default = "default_use_builtin_detector_profile",
        skip_serializing_if = "is_true"
    )]
    pub use_builtin_defaults: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workaround_marker_literals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workaround_leading_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workaround_marker_regexes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracker_reference_regexes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_guard_regexes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_guard_constants: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_guard_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendored_path_markers: Vec<String>,
    /// Language/extension tokens the field-pattern detector scans for repeated
    /// struct/record fields (e.g. `rs`, `php`, `ts`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_pattern_scan_tokens: Vec<String>,
    /// Of the scanned tokens, those whose field declarations use the
    /// type-before-name syntax (`Type $name` / `Type name`). Tokens not listed
    /// here default to the name-before-type syntax (`name: Type`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_pattern_type_before_name_tokens: Vec<String>,
    /// Tokens whose source files embed test code that must be stripped before
    /// scanning (e.g. inline `#[cfg(test)]` modules).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_pattern_inline_test_strip_tokens: Vec<String>,
    /// Filename suffixes that mark a source file as test scaffolding, skipped by
    /// path-scanning detectors (e.g. `_test.rs`, `.test.ts`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_file_suffixes: Vec<String>,
    /// Language tokens the dead-guard detector applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dead_guard_languages: Vec<String>,
    /// Basenames whose guards run outside normal production runtime assumptions
    /// (e.g. `uninstall.php`, lifecycle entrypoints).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_basenames: Vec<String>,
    /// Basename suffixes for lifecycle files (e.g. `-smoke.php`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_basename_suffixes: Vec<String>,
    /// Path segments (directory names) that mark a file as lifecycle/test
    /// scaffolding (e.g. `migrations`, `tests`, `fixtures`, `smoke`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_path_segments: Vec<String>,
    /// Language tokens the deprecation-age detector applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deprecation_languages: Vec<String>,
    /// Ordered version sources used to resolve a component's current version.
    /// The first source that yields a parseable semver wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_sources: Vec<VersionSource>,
    /// File extensions (without dot) the repeated-array-literal-shape detector
    /// scans. Core ships no default; components opt in their
    /// associative-array-literal languages so the detector stays agnostic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repeated_literal_shape_extensions: Vec<String>,
}

/// How to resolve a component version from a file (language/ecosystem-agnostic).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VersionSource {
    /// A header/regex match in any file with the given extension directly under
    /// the component root (e.g. a plugin header `Version: X.Y.Z`).
    HeaderRegex {
        /// File extension (without dot) to scan at the component root.
        file_extension: String,
        /// Regex with a single capture group for the semver string.
        pattern: String,
    },
    /// A JSON manifest's top-level string field (e.g. a manifest `version`).
    JsonManifest {
        /// Manifest filename relative to the component root.
        file: String,
        /// Top-level key whose string value is the semver.
        key: String,
    },
}

impl Default for DetectorProfileConfig {
    fn default() -> Self {
        Self {
            use_builtin_defaults: true,
            workaround_marker_literals: Vec::new(),
            workaround_leading_markers: Vec::new(),
            workaround_marker_regexes: Vec::new(),
            tracker_reference_regexes: Vec::new(),
            version_guard_regexes: Vec::new(),
            version_guard_constants: Vec::new(),
            version_guard_languages: Vec::new(),
            vendored_path_markers: Vec::new(),
            field_pattern_scan_tokens: Vec::new(),
            field_pattern_type_before_name_tokens: Vec::new(),
            field_pattern_inline_test_strip_tokens: Vec::new(),
            test_file_suffixes: Vec::new(),
            dead_guard_languages: Vec::new(),
            lifecycle_basenames: Vec::new(),
            lifecycle_basename_suffixes: Vec::new(),
            lifecycle_path_segments: Vec::new(),
            deprecation_languages: Vec::new(),
            version_sources: Vec::new(),
            repeated_literal_shape_extensions: Vec::new(),
        }
    }
}

impl DetectorProfileConfig {
    pub fn is_empty(&self) -> bool {
        self.use_builtin_defaults
            && self.workaround_marker_literals.is_empty()
            && self.workaround_leading_markers.is_empty()
            && self.workaround_marker_regexes.is_empty()
            && self.tracker_reference_regexes.is_empty()
            && self.version_guard_regexes.is_empty()
            && self.version_guard_constants.is_empty()
            && self.version_guard_languages.is_empty()
            && self.vendored_path_markers.is_empty()
            && self.field_pattern_scan_tokens.is_empty()
            && self.field_pattern_type_before_name_tokens.is_empty()
            && self.field_pattern_inline_test_strip_tokens.is_empty()
            && self.test_file_suffixes.is_empty()
            && self.dead_guard_languages.is_empty()
            && self.lifecycle_basenames.is_empty()
            && self.lifecycle_basename_suffixes.is_empty()
            && self.lifecycle_path_segments.is_empty()
            && self.deprecation_languages.is_empty()
            && self.version_sources.is_empty()
            && self.repeated_literal_shape_extensions.is_empty()
    }

    fn merge(&mut self, other: &DetectorProfileConfig) {
        self.use_builtin_defaults = self.use_builtin_defaults && other.use_builtin_defaults;
        extend_unique(
            &mut self.workaround_marker_literals,
            &other.workaround_marker_literals,
        );
        extend_unique(
            &mut self.workaround_leading_markers,
            &other.workaround_leading_markers,
        );
        extend_unique(
            &mut self.workaround_marker_regexes,
            &other.workaround_marker_regexes,
        );
        extend_unique(
            &mut self.tracker_reference_regexes,
            &other.tracker_reference_regexes,
        );
        extend_unique(
            &mut self.version_guard_regexes,
            &other.version_guard_regexes,
        );
        extend_unique(
            &mut self.version_guard_constants,
            &other.version_guard_constants,
        );
        extend_unique(
            &mut self.version_guard_languages,
            &other.version_guard_languages,
        );
        extend_unique(
            &mut self.vendored_path_markers,
            &other.vendored_path_markers,
        );
        extend_unique(
            &mut self.field_pattern_scan_tokens,
            &other.field_pattern_scan_tokens,
        );
        extend_unique(
            &mut self.field_pattern_type_before_name_tokens,
            &other.field_pattern_type_before_name_tokens,
        );
        extend_unique(
            &mut self.field_pattern_inline_test_strip_tokens,
            &other.field_pattern_inline_test_strip_tokens,
        );
        extend_unique(&mut self.test_file_suffixes, &other.test_file_suffixes);
        extend_unique(&mut self.dead_guard_languages, &other.dead_guard_languages);
        extend_unique(&mut self.lifecycle_basenames, &other.lifecycle_basenames);
        extend_unique(
            &mut self.lifecycle_basename_suffixes,
            &other.lifecycle_basename_suffixes,
        );
        extend_unique(
            &mut self.lifecycle_path_segments,
            &other.lifecycle_path_segments,
        );
        extend_unique(
            &mut self.deprecation_languages,
            &other.deprecation_languages,
        );
        for source in &other.version_sources {
            if !self.version_sources.contains(source) {
                self.version_sources.push(source.clone());
            }
        }
        extend_unique(
            &mut self.repeated_literal_shape_extensions,
            &other.repeated_literal_shape_extensions,
        );
    }
}

fn default_use_builtin_detector_profile() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TestWiringConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<TestWiringPolicy>,
}

impl TestWiringConfig {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    fn merge(&mut self, other: &TestWiringConfig) {
        for policy in &other.policies {
            if !self
                .policies
                .iter()
                .any(|existing| existing.id == policy.id)
            {
                self.policies.push(policy.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestWiringPolicy {
    /// Stable policy id used when merging extension and component config.
    pub id: String,
    /// Source files to scan for explicit wiring markers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_path_globs: Vec<String>,
    /// Test files that need policy evaluation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_path_globs: Vec<String>,
    /// Test files discovered by the native test runner without explicit wiring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_discovered_test_path_globs: Vec<String>,
    /// Test support/helper files exempt from explicit wiring checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub support_test_path_globs: Vec<String>,
    /// Whether matched, non-exempt test files must be referenced by source markers.
    #[serde(default)]
    pub require_explicit_wiring: bool,
    /// Regex patterns proving source-to-test wiring. `{test_path}` is replaced
    /// with a regex-escaped repository-relative test path before matching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explicit_wiring_marker_patterns: Vec<String>,
    /// Report convention label.
    #[serde(default = "default_test_wiring_convention")]
    pub convention: String,
    /// Finding severity: `warning` or `info`.
    #[serde(default = "default_test_wiring_severity")]
    pub severity: String,
    /// Finding description template. Supports `{test_path}`.
    #[serde(default = "default_test_wiring_description")]
    pub description: String,
    /// Finding suggestion template. Supports `{test_path}`.
    #[serde(default = "default_test_wiring_suggestion")]
    pub suggestion: String,
}

fn default_test_wiring_convention() -> String {
    "test_wiring".to_string()
}

fn default_test_wiring_severity() -> String {
    "warning".to_string()
}

fn default_test_wiring_description() -> String {
    "Test file `{test_path}` is not wired into the configured test harness".to_string()
}

fn default_test_wiring_suggestion() -> String {
    "Wire `{test_path}` using the configured explicit wiring convention".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteExecutionSafetyConfig {
    /// Report convention label for remote execution preflight findings.
    #[serde(
        default = "default_remote_execution_preflight_convention",
        skip_serializing_if = "is_default_remote_execution_preflight_convention"
    )]
    pub convention: String,
    /// Markers that identify remote execution dispatch sites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_markers: Vec<String>,
    /// Markers that prove local arguments/paths are translated or rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_translation_markers: Vec<String>,
    /// Markers that identify caller-provided arguments entering remote commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_forward_markers: Vec<String>,
    /// Markers that prove required remote capabilities were declared/checked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_preflight_markers: Vec<String>,
    /// Markers that identify component-specific artifact capture requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_capture_markers: Vec<String>,
    /// Markers that prove captured artifacts carry a source snapshot/mirror contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_snapshot_markers: Vec<String>,
    /// Markers that prove selected extensions/tools are available remotely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_parity_markers: Vec<String>,
    /// Markers that identify remote dispatch sites accepting extension selectors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_selector_markers: Vec<String>,
    /// Markers that identify remotely reported artifact references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_report_markers: Vec<String>,
    /// Markers that prove reported artifacts are locally accessible or retrievable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_access_markers: Vec<String>,
}

fn default_remote_execution_preflight_convention() -> String {
    "remote_execution_preflight".to_string()
}

fn is_default_remote_execution_preflight_convention(value: &str) -> bool {
    value == default_remote_execution_preflight_convention()
}

impl Default for RemoteExecutionSafetyConfig {
    fn default() -> Self {
        Self {
            convention: default_remote_execution_preflight_convention(),
            dispatch_markers: Vec::new(),
            path_translation_markers: Vec::new(),
            argument_forward_markers: Vec::new(),
            capability_preflight_markers: Vec::new(),
            artifact_capture_markers: Vec::new(),
            artifact_snapshot_markers: Vec::new(),
            extension_parity_markers: Vec::new(),
            extension_selector_markers: Vec::new(),
            artifact_report_markers: Vec::new(),
            artifact_access_markers: Vec::new(),
        }
    }
}

impl RemoteExecutionSafetyConfig {
    pub fn is_empty(&self) -> bool {
        self.dispatch_markers.is_empty()
            && self.path_translation_markers.is_empty()
            && self.argument_forward_markers.is_empty()
            && self.capability_preflight_markers.is_empty()
            && self.artifact_capture_markers.is_empty()
            && self.artifact_snapshot_markers.is_empty()
            && self.extension_parity_markers.is_empty()
            && self.extension_selector_markers.is_empty()
            && self.artifact_report_markers.is_empty()
            && self.artifact_access_markers.is_empty()
    }

    fn merge(&mut self, other: &RemoteExecutionSafetyConfig) {
        if other.convention != default_remote_execution_preflight_convention() {
            self.convention = other.convention.clone();
        }
        extend_unique(&mut self.dispatch_markers, &other.dispatch_markers);
        extend_unique(
            &mut self.path_translation_markers,
            &other.path_translation_markers,
        );
        extend_unique(
            &mut self.argument_forward_markers,
            &other.argument_forward_markers,
        );
        extend_unique(
            &mut self.capability_preflight_markers,
            &other.capability_preflight_markers,
        );
        extend_unique(
            &mut self.artifact_capture_markers,
            &other.artifact_capture_markers,
        );
        extend_unique(
            &mut self.artifact_snapshot_markers,
            &other.artifact_snapshot_markers,
        );
        extend_unique(
            &mut self.extension_parity_markers,
            &other.extension_parity_markers,
        );
        extend_unique(
            &mut self.extension_selector_markers,
            &other.extension_selector_markers,
        );
        extend_unique(
            &mut self.artifact_report_markers,
            &other.artifact_report_markers,
        );
        extend_unique(
            &mut self.artifact_access_markers,
            &other.artifact_access_markers,
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ArtifactPortabilityConfig {
    /// Number of recent observation runs to scan for persisted artifact path portability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_run_window: Option<usize>,
    /// Path prefixes that identify local/runtime-only locations in stored artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_portable_path_prefixes: Vec<String>,
    /// Path substrings that identify project-specific local/runtime-only artifact locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_portable_path_contains: Vec<String>,
}

impl ArtifactPortabilityConfig {
    pub fn is_empty(&self) -> bool {
        self.observation_run_window.is_none()
            && self.non_portable_path_prefixes.is_empty()
            && self.non_portable_path_contains.is_empty()
    }

    pub fn with_generic_defaults(&self) -> Self {
        let mut config = self.clone();
        extend_unique(
            &mut config.non_portable_path_prefixes,
            &[
                "/tmp/".to_string(),
                "/private/tmp/".to_string(),
                "/var/folders/".to_string(),
            ],
        );
        config
    }

    fn merge(&mut self, other: &ArtifactPortabilityConfig) {
        if other.observation_run_window.is_some() {
            self.observation_run_window = other.observation_run_window;
        }
        extend_unique(
            &mut self.non_portable_path_prefixes,
            &other.non_portable_path_prefixes,
        );
        extend_unique(
            &mut self.non_portable_path_contains,
            &other.non_portable_path_contains,
        );
    }
}

/// One named group of orchestration markers contributing to a command module's
/// orchestration weight. Core treats every marker as an opaque regex; the
/// component owns all ecosystem/language semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinCommandAdapterMarkerGroup {
    /// Human-readable category surfaced in findings (e.g. "process execution").
    pub label: String,
    /// Regex patterns whose matches count as orchestration evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
    /// Per-match weight for this group. Defaults to 1.
    #[serde(
        default = "default_thin_command_adapter_group_weight",
        skip_serializing_if = "is_default_thin_command_adapter_group_weight"
    )]
    pub weight: u32,
}

fn default_thin_command_adapter_group_weight() -> u32 {
    1
}

fn is_default_thin_command_adapter_group_weight(value: &u32) -> bool {
    *value == default_thin_command_adapter_group_weight()
}

/// Configurable thin-command-adapter detector policy.
///
/// The detector scans files within `include_path_contains` (and matching
/// `file_extensions`) and sums orchestration weight from configured marker
/// groups. A module whose orchestration weight reaches `max_orchestration_weight`
/// is flagged as too thick — its orchestration belongs in a core service.
///
/// Core stays ecosystem-agnostic: it never hardcodes language, framework, or
/// project terms. All markers, scopes, and allowlists come from config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinCommandAdapterConfig {
    /// Report convention label for findings.
    #[serde(
        default = "default_thin_command_adapter_convention",
        skip_serializing_if = "is_default_thin_command_adapter_convention"
    )]
    pub convention: String,
    /// Path substrings that scope which files are command-layer adapters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_path_contains: Vec<String>,
    /// Path substrings that exclude transitional modules until their extraction
    /// issues land.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
    /// Skip recognized test files/paths. Test scaffolding is not a command
    /// adapter, so it is excluded by default. Set false to scan test modules too.
    #[serde(
        default = "default_thin_command_adapter_skip_tests",
        skip_serializing_if = "is_true"
    )]
    pub skip_test_paths: bool,
    /// File extensions (without dot) the detector scans. Empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Line substrings that exempt a single line from contributing weight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_line_contains: Vec<String>,
    /// Line prefixes ignored entirely (e.g. comment markers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_line_prefixes: Vec<String>,
    /// When a line equals one of these (trimmed), the remainder of the file is
    /// ignored (e.g. an inline test module marker).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_after_line_equals: Vec<String>,
    /// Orchestration marker groups whose matches accumulate weight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orchestration_markers: Vec<ThinCommandAdapterMarkerGroup>,
    /// Orchestration weight (inclusive) at which a module is flagged. Defaults
    /// to 1, meaning any configured orchestration marker is a violation.
    #[serde(
        default = "default_thin_command_adapter_threshold",
        skip_serializing_if = "is_default_thin_command_adapter_threshold"
    )]
    pub max_orchestration_weight: u32,
}

fn default_thin_command_adapter_convention() -> String {
    "thin_command_adapter".to_string()
}

fn is_default_thin_command_adapter_convention(value: &str) -> bool {
    value == default_thin_command_adapter_convention()
}

fn default_thin_command_adapter_threshold() -> u32 {
    1
}

fn default_thin_command_adapter_skip_tests() -> bool {
    true
}

fn is_default_thin_command_adapter_threshold(value: &u32) -> bool {
    *value == default_thin_command_adapter_threshold()
}

impl Default for ThinCommandAdapterConfig {
    fn default() -> Self {
        Self {
            convention: default_thin_command_adapter_convention(),
            include_path_contains: Vec::new(),
            exclude_path_contains: Vec::new(),
            skip_test_paths: default_thin_command_adapter_skip_tests(),
            file_extensions: Vec::new(),
            allow_line_contains: Vec::new(),
            ignore_line_prefixes: Vec::new(),
            ignore_after_line_equals: Vec::new(),
            orchestration_markers: Vec::new(),
            max_orchestration_weight: default_thin_command_adapter_threshold(),
        }
    }
}

impl ThinCommandAdapterConfig {
    pub fn is_empty(&self) -> bool {
        self.include_path_contains.is_empty() && self.orchestration_markers.is_empty()
    }

    fn merge(&mut self, other: &ThinCommandAdapterConfig) {
        if other.convention != default_thin_command_adapter_convention() {
            self.convention = other.convention.clone();
        }
        extend_unique(
            &mut self.include_path_contains,
            &other.include_path_contains,
        );
        extend_unique(
            &mut self.exclude_path_contains,
            &other.exclude_path_contains,
        );
        if !other.skip_test_paths {
            self.skip_test_paths = false;
        }
        extend_unique(&mut self.file_extensions, &other.file_extensions);
        extend_unique(&mut self.allow_line_contains, &other.allow_line_contains);
        extend_unique(&mut self.ignore_line_prefixes, &other.ignore_line_prefixes);
        extend_unique(
            &mut self.ignore_after_line_equals,
            &other.ignore_after_line_equals,
        );
        extend_unique(
            &mut self.orchestration_markers,
            &other.orchestration_markers,
        );
        if other.max_orchestration_weight != default_thin_command_adapter_threshold() {
            self.max_orchestration_weight = other.max_orchestration_weight;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CommandStatusContractConfig {
    /// Visible command paths that must have at least one golden output fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_commands: Vec<String>,
    /// Visible command paths that must declare a validation-error scenario using
    /// `--output`, proving error responses still write the structured file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_output_error_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<CommandStatusContractScenario>,
}

impl CommandStatusContractConfig {
    pub fn is_empty(&self) -> bool {
        self.required_commands.is_empty()
            && self.required_output_error_commands.is_empty()
            && self.scenarios.is_empty()
    }

    fn merge(&mut self, other: &CommandStatusContractConfig) {
        for command in &other.required_commands {
            if !self.required_commands.contains(command) {
                self.required_commands.push(command.clone());
            }
        }
        for command in &other.required_output_error_commands {
            if !self.required_output_error_commands.contains(command) {
                self.required_output_error_commands.push(command.clone());
            }
        }
        for scenario in &other.scenarios {
            if !self
                .scenarios
                .iter()
                .any(|existing| existing.id == scenario.id)
            {
                self.scenarios.push(scenario.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandStatusContractScenario {
    /// Stable scenario id shown in findings.
    pub id: String,
    /// Visible command path this fixture covers, e.g. `audit` or `runs list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// JSON fixture path relative to the component root.
    pub file: String,
    /// Scenario outcome class. `validation_error` requires a failed envelope
    /// with an error object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Whether this scenario is expected to cover the global `--output` file path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_file: bool,
    /// Expected JSON Pointer fields and values, e.g. `/success: true`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expected_fields: BTreeMap<String, serde_json::Value>,
    /// Expected status label for declared status fields, e.g. `planned`,
    /// `skipped`, `empty`, `failed`, or `completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<String>,
    /// JSON Pointer fields that must equal `expected_status`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_fields: Vec<String>,
    /// Expected dry-run value for declared dry-run fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_dry_run: Option<bool>,
    /// JSON Pointer fields that must equal `expected_dry_run`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dry_run_fields: Vec<String>,
    /// Expected top-level Homeboy envelope success value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_success: Option<bool>,
    /// This scenario intentionally represents empty/no-op work that should
    /// succeed rather than report an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub empty_success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ConfigKeyUsageConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ConfigKeyUsageRule>,
}

impl ConfigKeyUsageConfig {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn merge(&mut self, other: &ConfigKeyUsageConfig) {
        for rule in &other.rules {
            if !self.rules.iter().any(|existing| existing.id == rule.id) {
                self.rules.push(rule.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigKeyUsageRule {
    /// Stable rule label used in finding descriptions and merge de-duplication.
    pub id: String,
    /// Optional path substrings excluded from all evidence collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
    /// Regexes that capture keys written or migrated into storage/builders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_patterns: Vec<ConfigKeyUsagePattern>,
    /// Regexes that capture accessors/backing helpers for keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessor_patterns: Vec<ConfigKeyUsagePattern>,
    /// Regexes that capture non-test runtime/display reads of keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_patterns: Vec<ConfigKeyUsagePattern>,
    /// Optional regex templates that match references to accessor symbols.
    /// `{symbol}` is replaced with the escaped captured accessor symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessor_symbol_read_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigKeyUsagePattern {
    pub pattern: String,
    #[serde(default = "default_config_key_capture")]
    pub key_capture: String,
    /// Optional symbol capture for accessor definitions. If present, core also
    /// treats non-test references to that symbol outside the definition file as reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_capture: Option<String>,
}

fn default_config_key_capture() -> String {
    "key".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PublicRegistryExposureConfig {
    /// Substrings that identify public endpoint/route declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_markers: Vec<String>,
    /// Substrings that identify public/no-auth permission callbacks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_access_markers: Vec<String>,
    /// Regexes for raw registry/config/status getter calls that expose metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_getter_patterns: Vec<String>,
    /// Regexes for permission-aware resolver/helper calls or type names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_aware_resolver_patterns: Vec<String>,
    /// Number of lines on either side of a raw getter that must contain the
    /// configured route and public-access markers. Defaults to the detector's
    /// conservative local window when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_context_lines: Option<usize>,
    /// Path substrings that explicitly identify files eligible to satisfy the
    /// resolver companion signal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolver_path_contains: Vec<String>,
    /// Whether files in the same namespace may satisfy the resolver companion
    /// signal. Disabled by default so proximity stays explicit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub resolver_same_namespace: bool,
    /// Path substrings that intentionally expose public discovery metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_path_contains: Vec<String>,
    /// Line substrings that intentionally allow a raw getter in a public route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_line_contains: Vec<String>,
}

impl PublicRegistryExposureConfig {
    pub fn is_empty(&self) -> bool {
        self.route_markers.is_empty()
            && self.public_access_markers.is_empty()
            && self.raw_getter_patterns.is_empty()
            && self.permission_aware_resolver_patterns.is_empty()
            && self.route_context_lines.is_none()
            && self.resolver_path_contains.is_empty()
            && !self.resolver_same_namespace
            && self.allow_path_contains.is_empty()
            && self.allow_line_contains.is_empty()
    }

    fn merge(&mut self, other: &PublicRegistryExposureConfig) {
        extend_unique(&mut self.route_markers, &other.route_markers);
        extend_unique(
            &mut self.public_access_markers,
            &other.public_access_markers,
        );
        extend_unique(&mut self.raw_getter_patterns, &other.raw_getter_patterns);
        extend_unique(
            &mut self.permission_aware_resolver_patterns,
            &other.permission_aware_resolver_patterns,
        );
        if other.route_context_lines.is_some() {
            self.route_context_lines = other.route_context_lines;
        }
        extend_unique(
            &mut self.resolver_path_contains,
            &other.resolver_path_contains,
        );
        self.resolver_same_namespace |= other.resolver_same_namespace;
        extend_unique(&mut self.allow_path_contains, &other.allow_path_contains);
        extend_unique(&mut self.allow_line_contains, &other.allow_line_contains);
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RedirectValidationConfig {
    /// Line substrings that identify configured request parameter names whose
    /// values may become redirect destinations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_names: Vec<String>,
    /// Line substrings that identify reads from request/user-input sources.
    /// Components or extensions own ecosystem-specific source syntax; core only
    /// matches configured markers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_source_markers: Vec<String>,
    /// Regex patterns that identify reads from request/user-input sources.
    /// Invalid patterns are ignored by the detector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_source_patterns: Vec<String>,
    /// Function names, method names, or line substrings that perform redirects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_sinks: Vec<String>,
    /// Function names, method names, or line substrings that validate/allowlist
    /// redirect destinations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_markers: Vec<String>,
    /// Optional path-extension filter, without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Path substrings that opt files out of this detector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
}

impl RedirectValidationConfig {
    pub fn is_empty(&self) -> bool {
        self.request_names.is_empty()
            && self.request_source_markers.is_empty()
            && self.request_source_patterns.is_empty()
            && self.redirect_sinks.is_empty()
            && self.validation_markers.is_empty()
            && self.file_extensions.is_empty()
            && self.exclude_path_contains.is_empty()
    }

    fn merge(&mut self, other: &RedirectValidationConfig) {
        extend_unique(&mut self.request_names, &other.request_names);
        extend_unique(
            &mut self.request_source_markers,
            &other.request_source_markers,
        );
        extend_unique(
            &mut self.request_source_patterns,
            &other.request_source_patterns,
        );
        extend_unique(&mut self.redirect_sinks, &other.redirect_sinks);
        extend_unique(&mut self.validation_markers, &other.validation_markers);
        extend_unique(&mut self.file_extensions, &other.file_extensions);
        extend_unique(
            &mut self.exclude_path_contains,
            &other.exclude_path_contains,
        );
    }
}

/// Extension-supplied call-name lists for the parallel-implementation /
/// duplication detector.
///
/// These augment — they do not replace — the built-in generic floors
/// (`to_string`, `clone`, `unwrap`, etc.) hard-coded in core. Core never
/// inspects these strings; it just merges them into the filter sets it
/// already uses on call sequences.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DuplicationDetectorConfig {
    /// Function/method names treated as trivial — too generic to carry
    /// workflow signal in the host language/framework. Merged with the
    /// built-in generic list (to_string, clone, len, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trivial_calls: Vec<String>,
    /// Function/method names treated as plumbing — useful in a body but
    /// too generic to flag as parallel implementation. Merged with the
    /// built-in plumbing list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plumbing_calls: Vec<String>,
}

impl DuplicationDetectorConfig {
    pub fn is_empty(&self) -> bool {
        self.trivial_calls.is_empty() && self.plumbing_calls.is_empty()
    }

    fn merge(&mut self, other: &DuplicationDetectorConfig) {
        extend_unique(&mut self.trivial_calls, &other.trivial_calls);
        extend_unique(&mut self.plumbing_calls, &other.plumbing_calls);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CoreBoundaryLeakConfig {
    /// Language, framework, runtime, tool, or extension identifiers that should
    /// not become first-class concepts in the configured core source paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<String>,
    /// Path substrings that identify core-owned source to scan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scan_path_contains: Vec<String>,
    /// Path substrings that are intentionally exempt, such as generated data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_path_contains: Vec<String>,
    /// Line substrings that explicitly mark a local example as allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_line_contains: Vec<String>,
    /// Path substrings treated as example-only when not otherwise allowlisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub example_path_contains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePolicyRule {
    /// Stable rule id shown in diagnostics and used for config merging.
    pub id: String,
    /// Audit finding kind in snake_case.
    #[serde(default = "default_source_policy_kind")]
    pub kind: String,
    /// `warning` or `info`. Defaults to `warning`.
    #[serde(default = "default_source_policy_severity")]
    pub severity: String,
    /// Report convention label. Defaults to `source_policy`.
    #[serde(default = "default_source_policy_convention")]
    pub convention: String,
    /// Optional language filter using Homeboy's lowercase language names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional path-extension filter, without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Path substrings that identify source to scan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_path_contains: Vec<String>,
    /// Path substrings that opt files out of this policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
    /// Line substrings that explicitly mark a local exception as allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_line_contains: Vec<String>,
    /// Trimmed line prefixes skipped before matching, such as comment prefixes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_line_prefixes: Vec<String>,
    /// Exact trimmed lines after which the rest of the file is ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_after_line_equals: Vec<String>,
    /// Path substrings treated as example-only when not otherwise allowlisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub example_path_contains: Vec<String>,
    /// Optional classification used for example paths. Defaults to `example-only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_classification: Option<String>,
    /// Description template. Supports `{term}`, `{line}`, `{classification}`, and `{context}`.
    pub description: String,
    /// Suggested action template. Supports `{term}`, `{line}`, `{classification}`, and `{context}`.
    pub suggestion: String,
    /// Source policy body. Core owns execution; components own terms and labels.
    #[serde(flatten)]
    pub rule: SourcePolicyRuleBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourcePolicyRuleBody {
    /// Emit one finding for configured terms in scoped source lines.
    ForbiddenTerms {
        terms: Vec<SourcePolicyTerm>,
        #[serde(default)]
        default_match: SourcePolicyMatchMode,
        #[serde(default = "default_source_policy_case_insensitive")]
        case_insensitive: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePolicyTerm {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<SourcePolicyMatchMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourcePolicyMatchMode {
    /// Match token-bounded identifiers.
    #[default]
    Token,
    /// Match the escaped term literally.
    Literal,
    /// Treat the term value as a regex pattern.
    Regex,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MutatingResourceAccessConfig {
    /// Source markers that identify files containing runtime handler registrations.
    /// Examples are framework-specific registration function names. Core treats
    /// them as opaque substrings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handler_registration_markers: Vec<String>,
    /// Markers that identify mutating routes/handlers, such as HTTP method
    /// constants, command verbs, or operation labels. Core treats them as opaque
    /// substrings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutating_operation_markers: Vec<String>,
    /// Regexes that identify resource IDs used by handlers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_identifier_patterns: Vec<String>,
    /// Substrings that identify direct ownership/access checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_helper_markers: Vec<String>,
    /// Substrings that identify trusted delegation paths known by the component
    /// to enforce ownership/access checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_delegation_markers: Vec<String>,
    /// Substrings that identify resource mutation inside a handler body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutator_markers: Vec<String>,
}

impl MutatingResourceAccessConfig {
    pub fn is_empty(&self) -> bool {
        self.handler_registration_markers.is_empty()
            && self.mutating_operation_markers.is_empty()
            && self.resource_identifier_patterns.is_empty()
            && self.access_helper_markers.is_empty()
            && self.trusted_delegation_markers.is_empty()
            && self.mutator_markers.is_empty()
    }

    fn merge(&mut self, other: &MutatingResourceAccessConfig) {
        extend_unique(
            &mut self.handler_registration_markers,
            &other.handler_registration_markers,
        );
        extend_unique(
            &mut self.mutating_operation_markers,
            &other.mutating_operation_markers,
        );
        extend_unique(
            &mut self.resource_identifier_patterns,
            &other.resource_identifier_patterns,
        );
        extend_unique(
            &mut self.access_helper_markers,
            &other.access_helper_markers,
        );
        extend_unique(
            &mut self.trusted_delegation_markers,
            &other.trusted_delegation_markers,
        );
        extend_unique(&mut self.mutator_markers, &other.mutator_markers);
    }
}

impl CoreBoundaryLeakConfig {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.scan_path_contains.is_empty()
            && self.allow_path_contains.is_empty()
            && self.allow_line_contains.is_empty()
            && self.example_path_contains.is_empty()
    }

    fn merge(&mut self, other: &CoreBoundaryLeakConfig) {
        extend_unique(&mut self.terms, &other.terms);
        extend_unique(&mut self.scan_path_contains, &other.scan_path_contains);
        extend_unique(&mut self.allow_path_contains, &other.allow_path_contains);
        extend_unique(&mut self.allow_line_contains, &other.allow_line_contains);
        extend_unique(
            &mut self.example_path_contains,
            &other.example_path_contains,
        );
    }
}

fn default_source_policy_kind() -> String {
    "source_policy_violation".to_string()
}

fn default_source_policy_severity() -> String {
    "warning".to_string()
}

fn default_source_policy_convention() -> String {
    "source_policy".to_string()
}

fn default_source_policy_case_insensitive() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConventionTagGlob {
    /// Opaque tag value. Core never interprets this string.
    pub tag: String,
    /// File globs that receive this tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct KnownSymbolsConfig {
    /// Header-version providers keyed by an extension-owned marker and header.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_versions: Vec<KnownSymbolHeaderVersionProvider>,
    /// Composer package providers keyed by package name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composer_packages: Vec<KnownSymbolPackageProvider>,
    /// Bootstrap path providers keyed by a normalized path substring or suffix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_paths: Vec<KnownSymbolBootstrapPathProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolHeaderVersionProvider {
    /// Marker used to locate the component entry file.
    pub file_marker: String,
    /// Header key whose value contains the runtime version floor.
    pub version_header: String,
    /// Symbols introduced by runtime version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolVersionedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolPackageProvider {
    pub package: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolBootstrapPathProvider {
    pub path_contains: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<KnownSymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolVersionedEntry {
    pub name: String,
    pub kind: KnownSymbolKind,
    pub introduced: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownSymbolEntry {
    pub name: String,
    pub kind: KnownSymbolKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KnownSymbolKind {
    Function,
    Class,
    Constant,
}

impl KnownSymbolsConfig {
    pub fn is_empty(&self) -> bool {
        self.header_versions.is_empty()
            && self.composer_packages.is_empty()
            && self.bootstrap_paths.is_empty()
    }

    fn merge(&mut self, other: &KnownSymbolsConfig) {
        extend_unique(&mut self.header_versions, &other.header_versions);
        extend_unique(&mut self.composer_packages, &other.composer_packages);
        extend_unique(&mut self.bootstrap_paths, &other.bootstrap_paths);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestedDetectorRule {
    /// Human-readable detector label used for logging/debugging.
    pub id: String,
    /// Audit finding kind in snake_case, e.g. `json_like_exact_match`.
    pub kind: String,
    /// `warning` or `info`. Defaults to `warning`.
    #[serde(default = "default_requested_detector_severity")]
    pub severity: String,
    /// Report convention label. Defaults to `requested_detectors`.
    #[serde(default = "default_requested_detector_convention")]
    pub convention: String,
    /// Optional language filter using Homeboy's lowercase language names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional path-extension filter, without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Path substrings that opt files out of this detector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
    /// Detector body. Core owns the execution primitives; extensions own the rules.
    #[serde(flatten)]
    pub rule: RequestedDetectorRuleBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestedDetectorRuleBody {
    /// Emit one finding for each regex match in a file.
    Regex {
        pattern: String,
        description: String,
        suggestion: String,
    },
    /// Emit regex findings only when extracted comments match a trigger pattern.
    CommentRegex {
        comment_pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment_exclude_pattern: Option<String>,
        pattern: String,
        description: String,
        suggestion: String,
    },
    /// Collect values with one regex, then flag matching literals in other files.
    DerivedLiteral {
        source_pattern: String,
        value_capture: String,
        label: String,
        literal_pattern: String,
        /// Optional extension-owned regexes matched against the candidate's source line.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude_match_context_patterns: Vec<String>,
        description: String,
        suggestion: String,
    },
    /// Flag files whose docs/schema claim a scoped internal proxy but whose
    /// implementation forwards request-controlled targets without an explicit
    /// allowlist/prefix marker. All markers are extension-owned regexes.
    ScopedProxy {
        claim_pattern: String,
        sink_pattern: String,
        target_pattern: String,
        allowlist_pattern: String,
        description: String,
        suggestion: String,
    },
    /// Emit a finding when a regex match is not accompanied by another regex in
    /// a configured text scope. Core does not interpret either pattern.
    RequiredRegex {
        pattern: String,
        required_pattern: String,
        #[serde(default)]
        required_scope: RequiredRegexScope,
        description: String,
        suggestion: String,
    },
    /// Collect values with one regex, then emit findings for values that do not
    /// have a corresponding required match elsewhere in the eligible corpus.
    DerivedAbsence {
        source_pattern: String,
        value_capture: String,
        label: String,
        required_pattern: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude_required_path_contains: Vec<String>,
        description: String,
        suggestion: String,
    },
    /// Compare configured import/export/copy key allowlists against keys that
    /// appear in behavior-bearing read/write sites. Core only compares captured
    /// strings; the component owns the regexes and runtime-key exclusions.
    ConfigRoundtripKeys {
        object: String,
        export_pattern: String,
        import_pattern: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        copy_patterns: Vec<String>,
        behavior_pattern: String,
        #[serde(default = "default_config_roundtrip_key_capture")]
        key_capture: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude_key_patterns: Vec<String>,
        description: String,
        suggestion: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequiredRegexScope {
    /// Search the whole file containing the candidate match.
    #[default]
    SameFile,
    /// Search only text before the candidate match in the same file.
    BeforeMatch,
    /// Search only text after the candidate match in the same file.
    AfterMatch,
    /// Search the full eligible file corpus.
    AnyEligibleFile,
}

fn default_requested_detector_severity() -> String {
    "warning".to_string()
}

fn default_requested_detector_convention() -> String {
    "requested_detectors".to_string()
}

fn default_config_roundtrip_key_capture() -> String {
    "key".to_string()
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

fn extend_unique<T: Clone + PartialEq>(target: &mut Vec<T>, source: &[T]) {
    for value in source {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
