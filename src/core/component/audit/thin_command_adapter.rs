use serde::{Deserialize, Serialize};

use super::extend_unique;

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
    /// Regex patterns; any line matching one is skipped entirely (does not
    /// contribute orchestration weight). Use for line shapes that are never
    /// orchestration, e.g. function declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_line_matches: Vec<String>,
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

fn is_true(value: &bool) -> bool {
    *value
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
            ignore_line_matches: Vec::new(),
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

    pub(super) fn merge(&mut self, other: &ThinCommandAdapterConfig) {
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
        extend_unique(&mut self.ignore_line_matches, &other.ignore_line_matches);
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
