use serde::{Deserialize, Serialize};

use super::extend_unique;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectorProfileConfig {
    /// Include Homeboy's built-in detector profile defaults. Components can set
    /// this to false when they provide a fully custom, non-default ecosystem profile.
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
    /// struct/record fields (component-supplied source extension tokens).
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
    /// (e.g. uninstall scripts, lifecycle entrypoints).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_basenames: Vec<String>,
    /// Basename suffixes for lifecycle files (e.g. a `-smoke` source suffix).
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

    pub(super) fn merge(&mut self, other: &DetectorProfileConfig) {
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
