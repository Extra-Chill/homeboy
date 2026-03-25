//! types — extracted from manifest.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use std::path::PathBuf;


/// Type of action that can be executed by a extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionType {
    Api,
    Command,
    Builtin,
}

/// Builtin action types for Desktop app (copy, export operations).
/// CLI parses these but does not execute them - Desktop implements the behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinAction {
    CopyColumn,
    ExportCsv,
    CopyJson,
}

/// HTTP method for API actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// Deploy lifecycle: verification rules, install overrides, version patterns, @since tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployCapability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<DeployVerification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<DeployOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_patterns: Vec<VersionPatternConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_tag: Option<SinceTagConfig>,
}

/// Test mapping convention: how source files map to test files.
/// Used by the audit pipeline for structural test coverage gap detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestMappingConfig {
    /// Source directories to scan (relative to component root).
    /// Example: `["src"]` for Rust, `["inc"]` for WordPress.
    pub source_dirs: Vec<String>,
    /// Test directories to scan (relative to component root).
    /// Example: `["tests"]` for both Rust and WordPress.
    pub test_dirs: Vec<String>,
    /// How source file paths map to test file paths.
    /// Template variables: `{dir}` (relative dir), `{name}` (filename without ext), `{ext}` (extension).
    /// Example Rust: `"tests/{dir}/{name}_test.{ext}"` or inline `#[cfg(test)]`
    /// Example WordPress: `"tests/Unit/{dir}/{name}Test.{ext}"`
    pub test_file_pattern: String,
    /// Prefix for test method names (e.g., `"test_"` for both Rust and PHP).
    #[serde(default = "default_test_prefix")]
    pub method_prefix: String,
    /// Whether the language uses inline tests (e.g., Rust `#[cfg(test)]` in the same file).
    #[serde(default)]
    pub inline_tests: bool,
    /// Directory path patterns that indicate high-priority test coverage.
    /// Files in matching directories get `Warning` severity instead of `Info`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_patterns: Vec<String>,
    /// Path patterns to exclude from test coverage checks entirely.
    /// Files matching any pattern are skipped for both missing_test_file and
    /// missing_test_method findings. Use for CLI wrappers, pure type definitions,
    /// and other structurally untestable code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_test_patterns: Vec<String>,
}

/// Docs audit: ignore patterns and feature detection patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCapability {
    /// Shell script that resolves reference dependencies and exports
    /// `HOMEBOY_AUDIT_REFERENCE_PATHS` (newline-separated directory paths).
    /// Reference dependencies are fingerprinted for cross-reference analysis
    /// (dead code detection) but excluded from convention and duplication detection.
    /// Example: WordPress core + plugin dependencies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_references: Option<String>,
    /// Glob patterns for paths to ignore during docs audit.
    /// Uses `*` for single segment and `**` for multiple segments (e.g., `/wp-json/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_claim_patterns: Vec<String>,
    /// Regex patterns to detect feature registrations in source code.
    /// Each pattern should have a capture group for the feature name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feature_patterns: Vec<String>,
    /// Human-readable labels for feature patterns, keyed by a substring of the pattern.
    /// Used by `docs generate --from-audit` to group and label features in generated docs.
    /// Example: `{"register_post_type": "Post Types", "register_rest_route": "REST API Routes"}`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_labels: HashMap<String, String>,
    /// Doc generation targets: maps a feature label to a file path and optional heading.
    /// Used by `docs generate --from-audit` to place features in the right doc files.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub doc_targets: HashMap<String, DocTarget>,
    /// Context extraction rules for feature patterns, keyed by a substring of the pattern.
    /// Tells the audit system what additional context to extract around each detected feature.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_context: HashMap<String, FeatureContextRule>,
    /// Test mapping convention for structural test coverage gap detection.
    /// Defines how source files map to test files and how methods map to test methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_mapping: Option<TestMappingConfig>,
}

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

/// Executable tool: runtime, inputs, and output schema.
/// Represents a extension that can be run as a standalone tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableCapability {
    pub runtime: RuntimeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,
}

/// Desktop/platform UI config: pinned files, database, discovery, commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_pinned_logs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

/// What a extension provides: file extensions it handles and capabilities it supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidesConfig {
    /// File extensions this extension can process (e.g., ["php", "inc"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_extensions: Vec<String>,
    /// Capabilities this extension supports (e.g., ["fingerprint", "refactor"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
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
    /// Language examples:
    /// - Rust: `cargo check`
    /// - PHP: `php -l` on each changed file
    /// - TypeScript: `tsc --noEmit`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validate: Option<String>,
    /// Script that formats source code after automated writes.
    /// Runs from the project root. Exit 0 on success, non-zero on failure.
    /// Formatting failure is non-fatal — it logs a warning but never rolls back.
    ///
    /// Language examples:
    /// - Rust: `cargo fmt`
    /// - TypeScript: `npx prettier --write .`
    /// - PHP: `vendor/bin/phpcbf`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Script that extracts function contracts from source files.
    /// Receives `{file, content}` JSON on stdin, outputs `{file, contracts: [...]}` JSON on stdout.
    /// Each contract describes a function's signature, control flow branches, effects, and calls.
    ///
    /// Used by the test generator, doc generator, and refactor safety checker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<DatabaseCliConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseCliConfig {
    pub tables_command: String,
    pub describe_command: String,
    pub query_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliHelpConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id_help: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_help: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliConfig {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir_template: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings_flags: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<CliHelpConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    pub find_command: String,
    pub base_path_transform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployVerification {
    pub path_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployOverride {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    pub install_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionPatternConfig {
    pub extension: String,
    pub pattern: String,
}

/// Configuration for replacing `@since` placeholder tags during version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinceTagConfig {
    /// File extensions to scan (e.g., [".php"]).
    pub extensions: Vec<String>,
    /// Regex pattern matching placeholder versions in `@since` tags.
    /// Default: `0\.0\.0|NEXT|TBD|TODO|UNRELEASED|x\.x\.x`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_build_script: Option<String>,
    /// Default artifact path pattern with template support.
    /// Supports: {component_id}, {local_path}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_pattern: Option<String>,
    /// Paths to clean up after successful deploy (e.g., node_modules, vendor, target)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Desktop app runtime type (python/shell/cli). CLI ignores this field.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub runtime_type: Option<String>,

    /// Shell command to execute when running the extension.
    /// Template variables: {{entrypoint}}, {{args}}, {{extensionPath}}, plus project context vars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,

    /// Shell command to set up the extension (e.g., create venv, install deps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,

    /// Shell command to check if extension is ready. Exit 0 = ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_check: Option<String>,

    /// Environment variables to set when running the extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// Entry point file (used in template substitution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,

    /// Default args template (used in template substitution).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,

    /// Default site for this extension (used by some CLI extensions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_site: Option<String>,

    /// Desktop app: Python dependencies to install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<String>>,

    /// Desktop app: Playwright browsers to install.
    #[serde(rename = "playwrightBrowsers", skip_serializing_if = "Option::is_none")]
    pub playwright_browsers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub input_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,
    pub arg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub schema: OutputSchema,
    pub display: String,
    pub selectable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionConfig {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: ActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<HttpMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_auth: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Builtin action type (Desktop app only). CLI parses but does not execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<BuiltinAction>,
    /// Column identifier for copy-column builtin action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub setting_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}
