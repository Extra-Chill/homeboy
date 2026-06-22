use serde::{Deserialize, Serialize};

use super::extend_unique;

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

    pub(super) fn merge(&mut self, other: &PublicRegistryExposureConfig) {
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

    pub(super) fn merge(&mut self, other: &RedirectValidationConfig) {
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

    pub(super) fn merge(&mut self, other: &DuplicationDetectorConfig) {
        extend_unique(&mut self.trivial_calls, &other.trivial_calls);
        extend_unique(&mut self.plumbing_calls, &other.plumbing_calls);
    }
}
