//! Test-mapping / test-drift configuration schema.
//!
//! `TestMappingConfig` (and the `BehaviorScenarioNames`, `IncludeWrapperPolicy`,
//! `TestVacuityPolicy`, `PackageNameSource` types it composes) describe how an
//! extension declares its source→test file conventions for the audit engine's
//! structural test-coverage and test-vacuity detectors. Pure serde config with
//! no dependencies; the `code_audit` engine consumes these and `extension`
//! re-exports them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_test_prefix() -> String {
    "test_".to_string()
}

/// Used by the audit pipeline for structural test coverage gap detection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestMappingConfig {
    /// Source directories to scan (relative to component root).
    pub source_dirs: Vec<String>,
    /// Test directories to scan (relative to component root).
    pub test_dirs: Vec<String>,
    /// How source file paths map to test file paths.
    /// Template variables: `{dir}` (relative dir), `{name}` (filename without ext), `{ext}` (extension).
    /// Extension manifests own the concrete template for each ecosystem.
    pub test_file_pattern: String,
    /// Prefix for test method names.
    #[serde(default = "default_test_prefix")]
    pub method_prefix: String,
    /// Whether the language uses inline tests in the same file.
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
    /// Regex templates for extracting test method names from a test file when
    /// the structural fingerprint does not list them, keyed by file extension.
    /// Each pattern must contain a single capture
    /// group for the method name and may use `{prefix}` for the configured
    /// method prefix (already regex-escaped). When no extension matches, the
    /// `default_test_method_pattern` is used. Extension manifests own the
    /// concrete per-language regexes so core stays language-agnostic.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub test_method_patterns: HashMap<String, String>,
    /// Fallback regex template used when no `test_method_patterns` entry matches
    /// the test file extension. May use `{prefix}` for the method prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_test_method_pattern: Option<String>,
    /// Name fragments that mark a test as describing a behavior/scenario rather
    /// than naming a specific source method. Used to suppress false
    /// orphaned-test findings for behavior-style test names. Extension manifests
    /// declare the idioms for their language/test framework.
    #[serde(default, skip_serializing_if = "BehaviorScenarioNames::is_empty")]
    pub behavior_scenario_names: BehaviorScenarioNames,
    /// Optional policy describing how a wrapper test file includes another test
    /// file (e.g. a path-mirrored module that only `include!`s the real test).
    /// When declared, the detector treats a wrapper that resolves to the
    /// expected test path as satisfying coverage. Language-specific include
    /// syntax is declared here rather than hardcoded in core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_wrapper: Option<IncludeWrapperPolicy>,
    /// Optional policy for detecting vacuous (no-op / placeholder) test methods.
    /// When declared, core runs the generic vacuity heuristics using the
    /// language markers supplied here; when absent, vacuity detection is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vacuity: Option<TestVacuityPolicy>,
    /// Method names that are universally idiomatic for this ecosystem (e.g.
    /// stdlib/trait methods, common accessors, framework lifecycle/magic
    /// methods). Methods whose name matches are not expected to carry a
    /// dedicated test. When empty, core falls back to its builtin agnostic set.
    /// Extension manifests own the concrete ecosystem literals so core stays
    /// language-agnostic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trivial_method_names: Vec<String>,
    /// Method-name prefixes that mark a method as a simple getter / predicate
    /// (e.g. `get_`, `is_`, `has_`). Methods whose name starts with one are
    /// treated as idiomatic and not expected to carry a dedicated test. When
    /// empty, core falls back to its builtin agnostic set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trivial_method_prefixes: Vec<String>,
}

impl TestMappingConfig {
    /// Resolve the effective set of universally-idiomatic method names for this
    /// config, falling back to the builtin agnostic set when the extension
    /// declared none. Returned as owned `String`s so callers can blend config
    /// and builtin defaults without core embedding the literals.
    pub fn effective_trivial_method_names(&self) -> Vec<String> {
        if self.trivial_method_names.is_empty() {
            homeboy_engine_primitives::language::Language::builtin_trivial_method_names()
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            self.trivial_method_names.clone()
        }
    }

    /// Resolve the effective set of idiomatic getter/predicate prefixes for this
    /// config, falling back to the builtin agnostic set when the extension
    /// declared none.
    pub fn effective_trivial_method_prefixes(&self) -> Vec<String> {
        if self.trivial_method_prefixes.is_empty() {
            homeboy_engine_primitives::language::Language::builtin_trivial_method_prefixes()
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            self.trivial_method_prefixes.clone()
        }
    }
}

/// Name fragments that indicate a behavior/scenario-describing test name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BehaviorScenarioNames {
    /// Prefixes that mark a behavior-describing test (e.g. `accepts_`,
    /// `detects_`, `rejects_`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<String>,
    /// Suffixes that mark a behavior-describing test (e.g. `_roundtrip`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suffixes: Vec<String>,
}

impl BehaviorScenarioNames {
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty() && self.suffixes.is_empty()
    }

    /// True when `name` matches any declared behavior-scenario prefix/suffix.
    pub fn matches(&self, name: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
            || self
                .suffixes
                .iter()
                .any(|suffix| name.ends_with(suffix.as_str()))
    }
}

/// Policy describing how a wrapper test file includes a real test file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeWrapperPolicy {
    /// File extensions (without dot) the wrapper convention applies to.
    pub file_extensions: Vec<String>,
    /// Template the wrapper file must equal (after whitespace removal) to count
    /// as an include of the target. `{relative_target}` is replaced with the
    /// target test path relative to the wrapper directory.
    pub include_template: String,
}

/// Policy for vacuity (no-op / placeholder) test detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestVacuityPolicy {
    /// File extensions (without dot) this policy applies to (e.g. `rs`).
    pub file_extensions: Vec<String>,
    /// Substrings that, when present in a test body, mark it as a deliberate
    /// compile/snapshot contract that should never be flagged as vacuous.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_body_markers: Vec<String>,
    /// Markers that indicate the test references product code (e.g. `crate::`,
    /// `super::`). A test that references product code is not vacuous.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub product_reference_markers: Vec<String>,
    /// Optional package-name resolver: a manifest filename plus the key whose
    /// value names the package, so `<package>::` also counts as a product
    /// reference. Lets core resolve a package name without knowing the
    /// ecosystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<PackageNameSource>,
}

/// How to resolve a package name from a manifest file (language-agnostic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageNameSource {
    /// Manifest filename relative to the component root (the ecosystem's
    /// package manifest).
    pub manifest_file: String,
    /// Optional section header that must precede the name entry (e.g. a
    /// `[section]` heading). When omitted the whole file is scanned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Key whose value names the package (e.g. `name`).
    pub key: String,
    /// When true, dashes in the resolved name are replaced with underscores
    /// (matches languages whose import path normalizes the package name).
    #[serde(default)]
    pub normalize_dashes_to_underscores: bool,
}
