use serde::{Deserialize, Serialize};

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
    /// Configurable ecosystem-term checks for core-owned source boundaries.
    #[serde(default, skip_serializing_if = "CoreBoundaryLeakConfig::is_empty")]
    pub core_boundary_leaks: CoreBoundaryLeakConfig,
    /// Extension-owned call-name lists used by the duplication /
    /// parallel-implementation detector to filter out language- and
    /// framework-specific noise. Core never interprets these strings; they
    /// are merged with the built-in generic floor lists.
    #[serde(default, skip_serializing_if = "DuplicationDetectorConfig::is_empty")]
    pub duplication_detector: DuplicationDetectorConfig,
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
}

fn default_requested_detector_severity() -> String {
    "warning".to_string()
}

fn default_requested_detector_convention() -> String {
    "requested_detectors".to_string()
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
            && self.core_boundary_leaks.is_empty()
            && self.duplication_detector.is_empty()
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
        self.duplication_detector.merge(&other.duplication_detector);
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
    fn dead_guard_comment_patterns_mark_audit_config_non_empty() {
        let config = AuditConfig {
            dead_guard_context_comment_patterns: vec!["dual context".to_string()],
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
}
