//! types — extracted from mod.rs.

use crate::component::Component;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCapability {
    Lint,
    Test,
    Build,
}

#[derive(Debug, Clone)]
pub struct ExtensionExecutionContext {
    pub component: Component,
    pub capability: ExtensionCapability,
    pub extension_id: String,
    pub extension_path: PathBuf,
    pub script_path: String,
    pub settings: Vec<(String, serde_json::Value)>,
}

/// A hook reference extracted from source code (do_action / apply_filters).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct HookRef {
    /// "action" or "filter"
    #[serde(rename = "type")]
    pub hook_type: String,
    /// The hook name (e.g., "woocommerce_product_is_visible")
    pub name: String,
}

/// A function parameter that is declared but never referenced in the function body.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct UnusedParam {
    /// The function/method name containing the unused parameter.
    pub function: String,
    /// The parameter name (without type annotations or sigils).
    pub param: String,
    /// Zero-based position of the parameter in the function signature.
    /// Used for call-site-aware analysis: compare against caller arg_count.
    #[serde(default)]
    pub position: usize,
}

/// A call site — a function/method invocation with argument count.
/// Used for cross-file parameter analysis (#824).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CallSite {
    /// The function/method name being called.
    pub target: String,
    /// The line number of the call (1-indexed).
    pub line: usize,
    /// The number of arguments passed at this call site.
    pub arg_count: usize,
}

/// A marker indicating the developer has acknowledged dead code
/// (e.g., `#[allow(dead_code)]` in Rust, `@codeCoverageIgnore` in PHP).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeadCodeMarker {
    /// The item name (function, struct, const, etc.) that is marked.
    pub item: String,
    /// The line number where the marker appears (1-indexed).
    pub line: usize,
    /// The type of marker (e.g., "allow_dead_code", "coverage_ignore", "phpstan_ignore").
    pub marker_type: String,
}

/// Output from a fingerprint extension script.
/// Matches the structural data extracted from a source file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FingerprintOutput {
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub type_name: Option<String>,
    /// All public type names found in the file (struct/class/enum names).
    /// Used for convention checks where the primary `type_name` may not
    /// be the convention-conforming type (e.g., a file with both
    /// `VersionOutput` and `VersionArgs` should not flag as a mismatch).
    #[serde(default)]
    pub type_names: Vec<String>,
    /// Parent class name (e.g., "WC_Abstract_Order").
    /// Separated from `implements` for clear hierarchy tracking.
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub implements: Vec<String>,
    #[serde(default)]
    pub registrations: Vec<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub imports: Vec<String>,
    /// Method name → normalized body hash for duplication detection.
    /// Extension scripts compute this by normalizing whitespace and hashing
    /// the function body. Optional — older scripts may not emit this.
    #[serde(default)]
    pub method_hashes: std::collections::HashMap<String, String>,
    /// Method name → structural hash for near-duplicate detection.
    /// Identifiers and literals are replaced with positional tokens before
    /// hashing, so functions with identical control flow but different
    /// variable names or constants produce the same hash.
    #[serde(default)]
    pub structural_hashes: std::collections::HashMap<String, String>,
    /// Method name → visibility ("public", "protected", "private").
    #[serde(default)]
    pub visibility: std::collections::HashMap<String, String>,
    /// Public/protected class properties (e.g., ["string $name", "$data"]).
    #[serde(default)]
    pub properties: Vec<String>,
    /// Hook references: do_action() and apply_filters() calls.
    #[serde(default)]
    pub hooks: Vec<HookRef>,
    /// Function parameters that are declared but never used in the function body.
    #[serde(default)]
    pub unused_parameters: Vec<UnusedParam>,
    /// Dead code suppression markers (e.g., `#[allow(dead_code)]`, `@codeCoverageIgnore`).
    #[serde(default)]
    pub dead_code_markers: Vec<DeadCodeMarker>,
    /// Function/method names called within this file (for cross-file reference analysis).
    #[serde(default)]
    pub internal_calls: Vec<String>,
    /// Call sites with argument counts (for cross-file parameter analysis).
    #[serde(default)]
    pub call_sites: Vec<CallSite>,
    /// Public functions/methods exported from this file (the file's API surface).
    #[serde(default)]
    pub public_api: Vec<String>,
}

/// Output from a `parse_items` refactor command.
/// Each item has boundaries, kind, name, visibility, and source text.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item (function, struct, enum, const, etc.).
    pub kind: String,
    /// Start line (1-indexed, includes doc comments and attributes).
    pub start_line: usize,
    /// End line (1-indexed, inclusive).
    pub end_line: usize,
    /// The extracted source code (including doc comments and attributes).
    pub source: String,
    /// Visibility: "pub", "pub(crate)", "pub(super)", or "" for private.
    #[serde(default)]
    pub visibility: String,
}

impl From<crate::extension::grammar_items::GrammarItem> for ParsedItem {
    fn from(gi: crate::extension::grammar_items::GrammarItem) -> Self {
        Self {
            name: gi.name,
            kind: gi.kind,
            start_line: gi.start_line,
            end_line: gi.end_line,
            source: gi.source,
            visibility: gi.visibility,
        }
    }
}

/// Output from a `resolve_imports` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedImports {
    /// Import statements needed in the destination file.
    pub needed_imports: Vec<String>,
    /// Warnings about imports that couldn't be resolved.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Output from a `find_related_tests` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelatedTests {
    /// Test items that should move with the extracted items.
    pub tests: Vec<ParsedItem>,
    /// Names of tests that reference multiple moved/unmoved items (can't cleanly move).
    #[serde(default)]
    pub ambiguous: Vec<String>,
}

/// Output from an `adjust_visibility` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdjustedItem {
    /// The item source with visibility adjusted.
    pub source: String,
    /// Whether visibility was changed.
    pub changed: bool,
    /// Original visibility.
    pub original_visibility: String,
    /// New visibility.
    pub new_visibility: String,
}

/// Output from a `rewrite_import_path` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RewrittenImport {
    /// Original import path.
    pub original: String,
    /// Corrected import path.
    pub rewritten: String,
    /// Whether the path changed.
    pub changed: bool,
}

/// Summary of an extension for list views.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_display_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
}

/// Summary of an extension action.
#[derive(Debug, Clone, Serialize)]
pub struct ActionSummary {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: ActionType,
}

/// Result of updating all extensions.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateAllResult {
    pub updated: Vec<UpdateEntry>,
    pub skipped: Vec<String>,
}

/// A single extension update entry with before/after versions.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateEntry {
    pub extension_id: String,
    pub old_version: String,
    pub new_version: String,
}
