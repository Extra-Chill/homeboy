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

/// What an extension provides: file extensions it handles and how it is discovered.
///
/// Unknown keys are ignored rather than rejected. This struct previously used
/// `deny_unknown_fields`, which made retiring any key a hard break: an older
/// published manifest carrying a since-removed key would fail to deserialize
/// entirely, taking `file_extensions` and `discovery_markers` down with it.
/// Extensions ship on their own release cadence, so tolerance here matches the
/// forward-compatibility policy the parent `ExtensionManifest` already sets
/// with its `#[serde(flatten)] extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidesConfig {
    /// File extensions this extension can process.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
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
///
/// Every field here must have a live consumer in core. A key with no reader is
/// not a feature, it is a promise the manifest cannot keep. `crossref` is the
/// cautionary case: the WordPress extension declared it, but it *never* existed
/// as a field on this struct, so serde silently dropped it on every parse for
/// the entire life of the key — leaving a 573-line script and a README row
/// wired to nothing. Nothing failed, nothing warned; the cost was only ever
/// visible to someone who went looking. It was removed at the source in
/// Extra-Chill/homeboy-extensions#2565 (key, script, and README row).
/// `topology` was the mirror image — it existed here, and nothing ever
/// declared it — and was removed in #11124.
///
/// Unknown keys are dropped rather than rejected (no `deny_unknown_fields`),
/// which is the right call for forward compatibility but means a typo or a
/// retired key fails silently. Verifying a declaration reaches a reader is
/// therefore a review obligation, not something the type system does here.
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
pub struct ExtensionDiagnosticsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ExtensionToolDiagnosticDeclaration>,
}

impl ExtensionDiagnosticsConfig {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
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

/// Extension-owned remote recipe execution descriptor.
///
/// Typed since #13724. While this rode in `ExtensionManifest::extra` it had two
/// readers that disagreed: one `.ok()`-discarded malformed entries, the other
/// retained them so `runner recipe-providers` could report which declaration was
/// broken and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeRunProviderDescriptor {
    pub id: String,
    pub version: String,
    pub executable: String,
    pub command: Vec<String>,
}

/// One declared recipe-run provider, retaining entries that do not satisfy the
/// descriptor shape.
///
/// Rejecting a malformed entry at manifest load would fail the whole manifest
/// and collapse a precise per-provider diagnostic ("requires id, version,
/// executable, and argv beginning with executable") into "this extension's
/// manifest is invalid". The malformed value is preserved so inventory can name
/// the offending declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RecipeRunProviderDeclaration {
    Descriptor(Box<RecipeRunProviderDescriptor>),
    Malformed(serde_json::Value),
}

impl RecipeRunProviderDeclaration {
    /// Best-effort string field read that works for both variants, so
    /// diagnostics can name a provider that failed to satisfy the shape.
    pub fn declared_str(&self, field: &str) -> Option<String> {
        match self {
            RecipeRunProviderDeclaration::Descriptor(descriptor) => match field {
                "id" => Some(descriptor.id.clone()),
                "version" => Some(descriptor.version.clone()),
                "executable" => Some(descriptor.executable.clone()),
                _ => None,
            },
            RecipeRunProviderDeclaration::Malformed(value) => value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }
}
