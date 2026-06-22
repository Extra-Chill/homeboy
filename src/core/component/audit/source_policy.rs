use serde::{Deserialize, Serialize};

use super::extend_unique;

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

    pub(super) fn merge(&mut self, other: &CoreBoundaryLeakConfig) {
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

    pub(super) fn merge(&mut self, other: &MutatingResourceAccessConfig) {
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
