use serde::{Deserialize, Serialize};

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
