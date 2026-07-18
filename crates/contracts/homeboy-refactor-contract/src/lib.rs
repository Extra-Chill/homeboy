//! Shared refactor contract types for homeboy.
//!
//! Behavior-free data for the refactor subsystem: the autofix-**result** types
//! (`refactor --from lint/test/audit --write` output) and the transform-**rule**
//! input types (find/replace rules an extension declares). These live below core
//! so consumers — the refactor engine that produces/applies them and report
//! layers like the extension lint/test commands that carry and declare them — can
//! share the vocabulary without depending on the refactor engine's behavior.

use serde::{Deserialize, Serialize};

/// Applied-change reporting for a refactor run. `refactor --from lint/test/audit
/// --write` are the entrypoints for fixes; this keeps applied-change reporting in
/// one place so commands don't invent parallel output models.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedRefactor {
    pub files_modified: usize,
    pub rerun_recommended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
}

/// Aggregated summary of the fixes applied in a refactor run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResultsSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub rules: Vec<RuleFixCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primitives: Vec<PrimitiveFixCount>,
}

/// Count of fixes applied for a single rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFixCount {
    pub rule: String,
    pub count: usize,
}

/// Count of fixes applied for a single refactor primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimitiveFixCount {
    pub primitive: String,
    pub count: usize,
}

/// A collection of transform rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformSet {
    /// Human-readable description of this transform set.
    #[serde(default)]
    pub description: String,
    /// Ordered list of rules to apply.
    pub rules: Vec<TransformRule>,
}

/// A single find/replace rule with a file glob filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformRule {
    /// Unique identifier within the set.
    pub id: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Regex pattern to find (supports capture groups).
    pub find: String,
    /// Replacement template. Supports `$1`, `$2`, `${name}` capture group refs,
    /// `$1:lower`/`:upper`/`:kebab`/`:snake`/`:pascal`/`:camel` case transforms,
    /// and `$$` for a literal dollar sign.
    ///
    /// Backslash escapes are collapsed before the template is handed to the
    /// regex engine: `\\` → one literal backslash, `\n` → newline, `\t` → tab,
    /// `\r` → CR, `\0` → nul, `\"` → `"`, `\'` → `'`. Unknown escapes pass
    /// through verbatim. This means that to emit a fully-qualified name like
    /// `\Example_Name` on disk, write `\\Example_Name` in JSON (which decodes
    /// to `\Example_Name` in memory — the literal `\` you want). See #1277.
    pub replace: String,
    /// Glob pattern for files to apply to (e.g., `tests/**/*.txt`).
    #[serde(default = "default_files_glob")]
    pub files: String,
    /// Match context: "line" (default) or "file" (whole-file regex, for multi-line).
    #[serde(default = "default_context")]
    pub context: String,
}

fn default_files_glob() -> String {
    "**/*".to_string()
}

fn default_context() -> String {
    "line".to_string()
}

/// Slim view of a refactor source-run, as the extension lint-command report
/// layer consumes it when formatting `refactor --from lint --write` output.
///
/// Carries only the fields the lint report reads, so `homeboy-core` can format
/// the report without depending on the refactor engine's full `RefactorSourceRun`
/// type. The refactor layer (via the CLI) builds this from its run result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LintFixInput {
    pub applied: bool,
    pub files_modified: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
}
