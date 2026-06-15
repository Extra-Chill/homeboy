use crate::core::engine::output_parse::ParseSpec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::manifest::TestDriftConfig;
use super::manifest_test_config::TestPassthroughFilter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvProviderConfig {
    /// Script path relative to the extension directory.
    ///
    /// The script runs with the same generic Homeboy execution context as the
    /// target command and prints a JSON object of environment variables to add.
    pub script: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_flags: Vec<CliAutoFlag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<CliHelpConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAutoFlag {
    #[serde(default)]
    pub when: CliAutoFlagCondition,
    pub flag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliAutoFlagCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    pub find_command: String,
    pub base_path_transform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployVerification {
    pub path_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_error_message: Option<String>,
}

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct DeployOwnerHint {
    pub path_contains: String,
    pub suggested_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePathInferenceRule {
    pub when_file_contains: FileContainsCondition,
    pub remote_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePathRootRule {
    pub path_prefix: String,
    pub root: String,
    #[serde(default)]
    pub strip_prefix: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detect_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileContainsCondition {
    pub file: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionPatternConfig {
    pub extension: String,
    pub pattern: String,
}

/// Configuration for replacing `@since` placeholder tags during version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinceTagConfig {
    /// File extensions to scan.
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
    /// Repo-relative paths to lockfiles this extension's build process
    /// regenerates.
    ///
    /// These are merge-aftermath drift on the base branch: a release version
    /// bump can cause extension-managed dependency metadata to refresh. The CI
    /// autofix pipeline treats lockfile drift the same as audit baseline drift:
    /// it's pushed directly to the base branch instead of opened as a
    /// reviewable PR.
    ///
    /// Paths are repo-root-relative. Absolute paths are rejected. Existence
    /// is the caller's responsibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lockfile_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,

    /// Changed-file routing rules for split lint runners.
    ///
    /// When present, changed-file lint scopes files to the matching runner step
    /// selectors instead of passing every changed file through one invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_file_routes: Vec<LintChangedFileRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintChangedFileRoute {
    /// File extensions matched without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,

    /// Glob patterns matched against component-relative file paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,

    /// Extension runner step selector.
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_parse: Option<ParseSpec>,
    /// Source/test selection contract used by changed-test and drift workflows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<TestDriftConfig>,

    /// Manifest-driven routing for changed-test selections before invoking the
    /// extension test runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_routing: Option<TestChangedFileRouting>,

    /// Manifest-driven mapping for Homeboy's generic `--filter` passthrough
    /// hint before invoking the extension test runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passthrough_filter: Option<TestPassthroughFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestChangedFileRouting {
    pub strategy: TestChangedFileRoutingStrategy,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_env: Option<TestChangedFileExclusiveEnv>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestChangedFileRoutingStrategy {
    FileArgs,
    RustCargo,
    ExclusiveEnv,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestChangedFileExclusiveEnv {
    /// Environment variable to set when all selected tests match this route.
    pub name: String,

    /// Glob patterns matched against component-relative test paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,

    /// File extensions matched without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_capabilities: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolchain_provenance: Vec<TraceToolchainProvenanceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceToolchainProvenanceConfig {
    pub id: String,
    pub label: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_field: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
}

/// Post-write verify contract for autofix. Runs from the component root after
/// `refactor --from ...` writes edits to disk. A non-zero exit code triggers a
/// full revert of the written files and marks every auto-applied fix as
/// declined (with the verify output captured on the chunk).
///
/// See #1167 for design rationale. Per-rule safety rails still live in the
/// fixers (see #1166); this is a general-purpose backstop that catches bugs
/// any individual rule's rails miss.
///
/// Typical extension configurations:
///
/// - Compile check: catches type errors after writes.
/// - Syntax check: validates changed files after writes.
/// - Generic: leave unset — verify is opt-in, absent config = no gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutofixVerifyConfig {
    /// Executable to run. Resolved against `PATH` unless absolute.
    pub command: String,

    /// Arguments passed to the command. Each entry is a distinct argv slot —
    /// no shell splitting. To pass multiple arguments as one string, put them
    /// in a single entry and wrap the full invocation in `sh -c` yourself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Maximum seconds to wait before killing the verify process. Defaults to
    /// 120 when absent. A verify that times out is treated as a failure —
    /// the same as a non-zero exit code — so the autofix reverts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl AutofixVerifyConfig {
    /// Effective timeout in seconds (120 when unset).
    pub fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(120)
    }
}
