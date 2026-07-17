//! Pure capability/runtime config contract types for extension manifests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Component environment detection supplied by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEnvConfig {
    /// Script path relative to the extension directory.
    /// Runs from the component root and emits runtime metadata as JSON.
    pub detect_script: String,
}

/// What a extension provides: file extensions it handles and capabilities it supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidesConfig {
    /// File extensions this extension can process.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validate: Option<String>,
    /// Script that formats source code after automated writes.
    /// Runs from the project root. Exit 0 on success, non-zero on failure.
    /// Formatting failure is non-fatal — it logs a warning but never rolls back.
    ///
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Script that collects compiler warnings.
    /// Runs from the project root and receives `{root}` JSON on stdin.
    /// Outputs `{warnings:[...]}` JSON using Homeboy's generic warning envelope.
    /// Split lint runners may use step selectors supplied by the extension.
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

/// Extension-declared release preflight.
///
/// Core schedules these before release mutation and executes the declared
/// extension action with the standard release payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePreflightConfig {
    /// Stable suffix for the generated plan step id.
    pub id: String,
    /// Human-readable plan label.
    pub label: String,
    /// Extension action id to execute for this preflight.
    pub action: String,
    /// Plan step ids that must complete before this preflight runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
}

/// Agent runtime package declarations supplied by an extension manifest.
///
/// These are intentionally provider-agnostic at the extension layer. Consumers
/// such as agent-task parse the provider-specific payloads they understand.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentRuntimeManifestConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_task_executors: Vec<serde_json::Value>,
    /// JSON field selectors in provider configuration payloads whose values are
    /// controller paths requiring Lab materialization. Selectors use dotted
    /// object keys and `[]`/`*` for array or map values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_path_fields: Vec<String>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ExtensionToolDiagnosticDeclaration {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
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
