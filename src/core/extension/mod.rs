pub mod bench;
pub mod build;
mod execution;
pub mod grammar;
pub mod grammar_items;
mod lifecycle;
pub mod lint;
mod manifest;
mod runner;
mod runner_contract;
mod runtime_helper;
mod scope;
pub mod test;
pub mod update_check;
pub mod version;

pub mod exec_context;

// Re-export runner types
pub use runner::{ExtensionRunner, RunnerOutput};
pub use runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, PhaseFailure,
    PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter, VerificationPhase,
};
pub use runtime_helper::RUNNER_STEPS_ENV;

// Re-export manifest types
pub use manifest::{
    ActionConfig, ActionType, AuditCapability, AutofixVerifyConfig, BenchConfig, BuildConfig,
    CliAutoFlag, CliAutoFlagCondition, CliConfig, CliHelpConfig, ComponentEnvConfig,
    DatabaseCliConfig, DatabaseConfig, DeployCapability, DeployOverride, DeployVerification,
    DiscoveryConfig, DocTarget, ExecutableCapability, ExtensionManifest, FeatureContextRule,
    FileContainsCondition, HttpMethod, InputConfig, LintConfig, OutputConfig, OutputSchema,
    PlatformCapability, ProvidesConfig, RemotePathInferenceRule, RequirementsConfig, RuntimeConfig,
    RuntimeRequirementsConfig, ScriptsConfig, SelectOption, SettingConfig, SinceTagConfig,
    TestConfig, TestMappingConfig, VersionPatternConfig,
};

// Re-export version types
pub use version::{parse_extension_version, VersionConstraint};

// Re-export execution types and functions
pub(crate) use execution::execute_action;
pub use execution::{
    extension_ready_status, is_extension_compatible, run_action, run_extension, run_setup,
    ExtensionExecutionMode, ExtensionReadyStatus, ExtensionRunResult, ExtensionSetupResult,
    ExtensionStepFilter,
};

// Re-export scope types
pub use scope::ExtensionScope;

// Re-export lifecycle types and functions
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, install_for_component, is_git_url,
    read_source_revision, slugify_id, uninstall, update, InstallForComponentResult, InstallResult,
    UpdateAvailable, UpdateResult,
};

// Re-export aggregate query types
// (ActionSummary, ExtensionSummary, UpdateAllResult, UpdateEntry defined below in this file)

// Extension loader functions

use crate::component::Component;
use crate::config;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use crate::paths;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    let mut manifest = config::load::<ExtensionManifest>(id)?;
    let extension_dir = paths::extension(id)?;
    manifest.extension_path = Some(extension_dir.to_string_lossy().to_string());
    Ok(manifest)
}

pub fn load_all_extensions() -> Result<Vec<ExtensionManifest>> {
    let extensions = config::list::<ExtensionManifest>()?;
    let mut extensions_with_paths = Vec::new();
    for mut extension in extensions {
        let extension_dir = paths::extension(&extension.id)?;
        extension.extension_path = Some(extension_dir.to_string_lossy().to_string());
        extensions_with_paths.push(extension);
    }
    Ok(extensions_with_paths)
}

pub fn find_extension_by_tool(tool: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions
            .into_iter()
            .find(|m| m.cli.as_ref().is_some_and(|c| c.tool == tool))
    })
}

/// Find a extension that handles a given file extension and has a specific capability script.
///
/// Looks through all installed extensions for one whose `provides.file_extensions` includes
/// the given extension and whose `scripts` has the requested capability configured.
///
/// Returns the extension manifest with `extension_path` populated.
pub fn find_extension_for_file_ext(ext: &str, capability: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions.into_iter().find(|m| {
            if !m.handles_file_extension(ext) {
                return false;
            }
            match capability {
                "fingerprint" => m.fingerprint_script().is_some(),
                "refactor" => m.refactor_script().is_some(),
                "audit" => m.test_mapping().is_some(),
                _ => false,
            }
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCapability {
    Lint,
    Test,
    Build,
    Bench,
}

impl ExtensionCapability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExtensionCapability::Lint => "lint",
            ExtensionCapability::Test => "test",
            ExtensionCapability::Build => "build",
            ExtensionCapability::Bench => "bench",
        }
    }

    pub(crate) fn has_manifest_support(self, manifest: &ExtensionManifest) -> bool {
        match self {
            ExtensionCapability::Lint => manifest.has_lint(),
            ExtensionCapability::Test => manifest.has_test(),
            ExtensionCapability::Build => manifest.has_build(),
            ExtensionCapability::Bench => manifest.has_bench(),
        }
    }

    pub(crate) fn script_path(self, manifest: &ExtensionManifest) -> Option<&str> {
        match self {
            ExtensionCapability::Lint => manifest.lint_script(),
            ExtensionCapability::Test => manifest.test_script(),
            ExtensionCapability::Build => manifest.build_script(),
            ExtensionCapability::Bench => manifest.bench_script(),
        }
    }

    pub(crate) fn requires_script(self) -> bool {
        self != ExtensionCapability::Build
    }
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

fn no_extensions_error(component: &Component) -> Error {
    let mut err = Error::validation_invalid_argument(
        "component",
        format!("Component '{}' has no extensions configured", component.id),
        None,
        None,
    );

    for hint in extension_guidance_hints(component, None) {
        err = err.with_hint(hint);
    }

    err
}

fn capability_missing_error(component: &Component, capability: ExtensionCapability) -> Error {
    let capability_name = capability.label();
    let mut err = Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has no linked extensions that provide {} support",
            component.id, capability_name
        ),
        None,
        None,
    );

    for hint in extension_guidance_hints(component, Some(capability)) {
        err = err.with_hint(hint);
    }

    err
}

pub(crate) fn extension_guidance_hints(
    component: &Component,
    capability: Option<ExtensionCapability>,
) -> Vec<String> {
    let link_hint = match capability {
        Some(capability) => format!(
            "Link an extension with {} support: homeboy component set {} --extension <extension_id>",
            capability.label(),
            component.id
        ),
        None => format!(
            "Link an extension that provides the needed command support: homeboy component set {} --extension <extension_id>",
            component.id
        ),
    };

    vec![
        link_hint,
        "List installed extensions: homeboy extension list".to_string(),
        "For one-off shell builds or checks, model the workflow as a rig `command` step; component-level `build_command` is not supported.".to_string(),
    ]
}

fn capability_ambiguous_error(
    component: &Component,
    capability: ExtensionCapability,
    matching: &[String],
) -> Error {
    let capability_name = capability.label();
    Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has multiple linked extensions with {} support: {}",
            component.id,
            capability_name,
            matching.join(", ")
        ),
        None,
        None,
    )
    .with_hint(format!(
        "Configure explicit {} extension ownership before running this command",
        capability_name
    ))
}

fn linked_extensions(
    component: &Component,
) -> Result<&HashMap<String, crate::component::ScopedExtensionConfig>> {
    component
        .extensions
        .as_ref()
        .ok_or_else(|| no_extensions_error(component))
}

pub fn extract_component_extension_settings(
    component: &Component,
    extension_id: &str,
) -> Vec<(String, serde_json::Value)> {
    component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(extension_id))
        .map(|extension_config| {
            extension_config
                .settings
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_extension_for_capability(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<String> {
    let extensions = linked_extensions(component)?;
    if extensions.is_empty() {
        return Err(no_extensions_error(component));
    }

    let mut matching = Vec::new();

    for extension_id in extensions.keys() {
        let manifest = load_extension(extension_id)?;
        if capability.has_manifest_support(&manifest) {
            matching.push(extension_id.clone());
        }
    }

    match matching.len() {
        0 => Err(capability_missing_error(component, capability)),
        1 => Ok(matching.remove(0)),
        _ => Err(capability_ambiguous_error(component, capability, &matching)),
    }
}

pub fn resolve_execution_context(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<ExtensionExecutionContext> {
    let extension_id = resolve_extension_for_capability(component, capability)?;
    let manifest = load_extension(&extension_id)?;
    let script_path = capability
        .script_path(&manifest)
        .map(|s| s.to_string())
        // Build's extension_script is optional (builds can use local scripts or command templates),
        // so we allow an empty script_path for Build. Lint/Test/Bench require it.
        .or_else(|| {
            if capability.requires_script() {
                None
            } else {
                Some(String::new())
            }
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have {} infrastructure configured",
                    extension_id,
                    capability.label()
                ),
                None,
                None,
            )
        })?;

    let extension_path = extension_path(&extension_id);

    if !extension_path.exists() {
        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' not found in ~/.config/homeboy/extensions/",
                extension_id
            ),
            None,
            None,
        ));
    }

    Ok(ExtensionExecutionContext {
        component: component.clone(),
        capability,
        extension_id: extension_id.clone(),
        extension_path,
        script_path,
        settings: extract_component_extension_settings(component, &extension_id),
    })
}

/// Run a extension's fingerprint script on file content.
///
/// The script receives a JSON object on stdin:
/// ```json
/// {"file_path": "src/core/foo.rs", "content": "...file content..."}
/// ```
///
/// The script must output a JSON object on stdout matching the FileFingerprint schema:
/// ```json
/// {
///   "methods": ["foo", "bar"],
///   "type_name": "MyStruct",
///   "implements": ["SomeTrait"],
///   "registrations": [],
///   "namespace": null,
///   "imports": ["crate::error::Result"]
/// }
/// ```
pub fn run_fingerprint_script(
    extension: &ExtensionManifest,
    file_path: &str,
    content: &str,
) -> Option<FingerprintOutput> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.fingerprint_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    let input = serde_json::json!({
        "file_path": file_path,
        "content": content,
    });

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh -c <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let output = std::process::Command::new(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
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
    /// Functions/methods registered as hook/callback targets from WITHIN
    /// this file. These names are invoked by the framework runtime (e.g.,
    /// WordPress's hook system, `register_activation_hook`, REST callbacks,
    /// block render callbacks), not by direct calls from other source files.
    ///
    /// When a file both defines a function AND registers it as a hook
    /// callback, the function IS live code — it's just invoked through the
    /// framework rather than through a direct function call. The dead-code
    /// check uses this field to suppress false positives on such functions.
    ///
    /// Populated by extension fingerprint scripts; older scripts may not
    /// emit it (defaults to empty Vec). Extensions should populate this for
    /// ALL framework-runtime invocation patterns they can detect in the
    /// language/framework they support — not just WordPress hooks.
    #[serde(default)]
    pub hook_callbacks: Vec<String>,
}

// ============================================================================
// Refactor Script Protocol
// ============================================================================

/// Run a extension's refactor script with a command.
///
/// The script receives a JSON command on stdin and outputs JSON on stdout.
/// Commands are dispatched by the `command` field. Each command has its own
/// input/output schema.
///
/// Supported commands:
/// - `parse_items`: Parse source file, return all top-level items with boundaries
/// - `resolve_imports`: Given moved items, resolve what imports the destination needs
/// - `adjust_visibility`: Adjust visibility of items crossing module boundaries
/// - `find_related_tests`: Find test functions related to named items
/// - `rewrite_import_path`: Compute the corrected import path for a moved item
pub fn run_refactor_script(
    extension: &ExtensionManifest,
    command: &serde_json::Value,
) -> Option<serde_json::Value> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.refactor_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh -c <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let output = std::process::Command::new(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(command.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            crate::log_status!("refactor", "Extension script error: {}", stderr.trim());
        }
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
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

pub fn extension_path(id: &str) -> PathBuf {
    paths::extension(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_extension_ids() -> Vec<String> {
    config::list_ids::<ExtensionManifest>().unwrap_or_default()
}

// ============================================================================
// Aggregate query functions
// ============================================================================

use serde::Serialize;

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

/// List all extensions with pre-computed summary fields.
///
/// Aggregates ready status, compatibility, linked status, CLI info, actions,
/// and runtime details into a single summary per extension.
pub fn list_summaries(project: Option<&crate::project::Project>) -> Vec<ExtensionSummary> {
    let extensions = load_all_extensions().unwrap_or_default();

    extensions
        .iter()
        .map(|ext| {
            let ready_status = extension_ready_status(ext);
            let compatible = is_extension_compatible(ext, project);
            let linked = is_extension_linked(&ext.id);

            let (cli_tool, cli_display_name) = ext
                .cli
                .as_ref()
                .map(|cli| (Some(cli.tool.clone()), Some(cli.display_name.clone())))
                .unwrap_or((None, None));

            let actions: Vec<ActionSummary> = ext
                .actions
                .iter()
                .map(|a| ActionSummary {
                    id: a.id.clone(),
                    label: a.label.clone(),
                    action_type: a.action_type.clone(),
                })
                .collect();

            let has_setup = ext
                .runtime()
                .and_then(|r| r.setup_command.as_ref())
                .map(|_| true);
            let has_ready_check = ext
                .runtime()
                .and_then(|r| r.ready_check.as_ref())
                .map(|_| true);

            let source_revision = read_source_revision(&ext.id);

            ExtensionSummary {
                id: ext.id.clone(),
                name: ext.name.clone(),
                version: ext.version.clone(),
                description: ext
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: if ext.executable.is_some() {
                    "executable".to_string()
                } else {
                    "platform".to_string()
                },
                compatible,
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                linked,
                path: ext.extension_path.clone().unwrap_or_default(),
                source_revision,
                cli_tool,
                cli_display_name,
                actions,
                has_setup,
                has_ready_check,
            }
        })
        .collect()
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

/// Update all installed extensions, skipping linked ones.
///
/// Linked extensions are managed externally (symlinks to dev directories)
/// and should not be updated via git pull.
pub fn update_all(force: bool) -> UpdateAllResult {
    let extension_ids = available_extension_ids();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for id in &extension_ids {
        if is_extension_linked(id) {
            skipped.push(id.clone());
            continue;
        }

        let old_version = load_extension(id).ok().map(|m| m.version.clone());

        match update(id, force) {
            Ok(_) => {
                let new_version = load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();

                updated.push(UpdateEntry {
                    extension_id: id.clone(),
                    old_version: old_version.unwrap_or_default(),
                    new_version,
                });
            }
            Err(_) => {
                skipped.push(id.clone());
            }
        }
    }

    UpdateAllResult { updated, skipped }
}

/// Execute a tool from an extension's vendor directory.
///
/// Sets up PATH with the extension's vendor/bin and node_modules/.bin,
/// resolves the working directory from an optional component, and runs
/// the command interactively.
pub fn exec_tool(extension_id: &str, component_id: Option<&str>, args: &[String]) -> Result<i32> {
    use crate::server::execute_local_command_interactive;

    let extension = load_extension(extension_id)?;
    let ext_path = extension
        .extension_path
        .as_deref()
        .ok_or_else(|| Error::config_missing_key("extension_path", Some(extension_id.into())))?;

    // Resolve working directory
    let working_dir = if let Some(cid) = component_id {
        let comp = crate::component::load(cid)?;
        comp.local_path.clone()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    };

    // Build PATH with extension vendor directories prepended
    let vendor_bin = format!("{}/vendor/bin", ext_path);
    let node_bin = format!("{}/node_modules/.bin", ext_path);
    let current_path = std::env::var("PATH").unwrap_or_default();
    let enriched_path = format!("{}:{}:{}", vendor_bin, node_bin, current_path);

    let env = vec![
        ("PATH", enriched_path.as_str()),
        (exec_context::EXTENSION_PATH, ext_path),
        (exec_context::EXTENSION_ID, extension_id),
    ];

    let command = args.join(" ");
    Ok(execute_local_command_interactive(
        &command,
        Some(&working_dir),
        Some(&env),
    ))
}

pub fn save_manifest(manifest: &ExtensionManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ExtensionManifest>(id, json_spec, replace_fields)
}

/// Check if a extension is a symlink (linked, not installed).
pub fn is_extension_linked(extension_id: &str) -> bool {
    paths::extension(extension_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}

/// Validate that all extensions declared in a component's `extensions` field are installed.
///
/// If `component.extensions` contains keys like `{"wordpress": {}}`, those extensions
/// are implicitly required. Returns an actionable error with install commands
/// when any are missing.
pub fn validate_required_extensions(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    let mut missing: Vec<String> = Vec::new();
    for extension_id in extensions.keys() {
        if load_extension(extension_id).is_err() {
            missing.push(extension_id.clone());
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort();

    let extension_list = missing.join(", ");
    let install_hints: Vec<String> = missing
        .iter()
        .map(|id| {
            format!(
                "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                id
            )
        })
        .collect();

    let message = if missing.len() == 1 {
        format!(
            "Component '{}' requires extension '{}' which is not installed",
            component.id, missing[0]
        )
    } else {
        format!(
            "Component '{}' requires extensions not installed: {}",
            component.id, extension_list
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "missing_extensions": missing,
        }),
    );

    for hint in &install_hints {
        err = err.with_hint(hint.to_string());
    }

    err = err.with_hint(
        "Browse available extensions: https://github.com/Extra-Chill/homeboy-extensions"
            .to_string(),
    );

    Err(err)
}

/// Validate that all extensions declared in a component's `extensions` field are installed
/// and satisfy the declared version constraints.
///
/// Returns an actionable error listing every unsatisfied requirement with install/update hints.
pub fn validate_extension_requirements(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(()),
    };

    let mut errors: Vec<String> = Vec::new();
    let mut hints: Vec<String> = Vec::new();

    for (extension_id, ext_config) in extensions {
        let constraint_str = match &ext_config.version {
            Some(v) => v.as_str(),
            None => continue, // No version constraint, skip validation
        };

        let constraint = match version::VersionConstraint::parse(constraint_str) {
            Ok(c) => c,
            Err(_) => {
                errors.push(format!(
                    "Invalid version constraint '{}' for extension '{}'",
                    constraint_str, extension_id
                ));
                continue;
            }
        };

        match load_extension(extension_id) {
            Ok(extension) => match extension.semver() {
                Ok(installed_version) => {
                    if !constraint.matches(&installed_version) {
                        errors.push(format!(
                            "'{}' requires {}, but {} is installed",
                            extension_id, constraint, installed_version
                        ));
                        hints.push(format!(
                            "Run `homeboy extension update {}` to get the latest version",
                            extension_id
                        ));
                    }
                }
                Err(_) => {
                    errors.push(format!(
                        "Extension '{}' has invalid version '{}'",
                        extension_id, extension.version
                    ));
                }
            },
            Err(_) => {
                errors.push(format!("Extension '{}' is not installed", extension_id));
                hints.push(format!(
                    "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                    extension_id
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    let message = if errors.len() == 1 {
        format!(
            "Component '{}' has an unsatisfied extension requirement: {}",
            component.id, errors[0]
        )
    } else {
        format!(
            "Component '{}' has {} unsatisfied extension requirements:\n  - {}",
            component.id,
            errors.len(),
            errors.join("\n  - ")
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "unsatisfied": errors,
        }),
    );

    for hint in &hints {
        err = err.with_hint(hint.to_string());
    }

    Err(err)
}

/// Check if any of the component's linked extensions provide build configuration.
pub fn extension_provides_build(component: &crate::component::Component) -> bool {
    let extensions = match &component.extensions {
        Some(m) => m,
        None => return false,
    };

    for extension_id in extensions.keys() {
        if let Ok(extension) = load_extension(extension_id) {
            if extension.has_build() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopedExtensionConfig};
    use std::collections::HashMap;

    #[test]
    fn extension_capability_owns_labels_and_scripts() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Example",
            "version": "0.0.0",
            "runtime": { "node": "24" },
            "lint": { "extension_script": "lint.sh" },
            "test": {
                "extension_script": "test.sh",
                "result_parse": {
                    "rules": [{ "pattern": "Tests: (\\d+)", "field": "total" }]
                }
            },
            "build": { "extension_script": "build.sh" },
            "bench": { "extension_script": "bench.sh" }
        }))
        .unwrap();

        assert_eq!(
            manifest
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.node.as_deref()),
            Some("24")
        );
        assert_eq!(
            manifest
                .test
                .as_ref()
                .and_then(|test| test.result_parse.as_ref())
                .map(|spec| spec.rules.len()),
            Some(1)
        );

        for (capability, label, script, requires_script) in [
            (ExtensionCapability::Lint, "lint", "lint.sh", true),
            (ExtensionCapability::Test, "test", "test.sh", true),
            (ExtensionCapability::Build, "build", "build.sh", false),
            (ExtensionCapability::Bench, "bench", "bench.sh", true),
        ] {
            assert_eq!(capability.label(), label);
            assert!(capability.has_manifest_support(&manifest));
            assert_eq!(capability.script_path(&manifest), Some(script));
            assert_eq!(capability.requires_script(), requires_script);
        }
    }

    #[test]
    fn validate_required_extensions_passes_with_no_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_passes_with_empty_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(HashMap::new()),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_fails_with_missing_module() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "nonexistent-extension-abc123".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ExtensionNotFound);
        assert!(err.message.contains("nonexistent-extension-abc123"));
        assert!(err.message.contains("test-component"));
        // Should have install hint + browse hint
        assert!(err.hints.len() >= 2);
        assert!(err
            .hints
            .iter()
            .any(|h| h.message.contains("homeboy extension install")));
        assert!(err
            .hints
            .iter()
            .any(|h| h.message.contains("homeboy-extensions")));
    }

    #[test]
    fn validate_required_extensions_reports_all_missing() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "missing-mod-a".to_string(),
            ScopedExtensionConfig::default(),
        );
        extensions.insert(
            "missing-mod-b".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "multi-dep".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        // Error should mention both missing extensions
        assert!(err.message.contains("missing-mod-a"));
        assert!(err.message.contains("missing-mod-b"));
        // Should have install hint for each + browse hint
        assert!(err.hints.len() >= 3);
    }

    #[test]
    fn extension_guidance_hints_point_to_supported_paths() {
        let comp = Component {
            id: "plain-package".to_string(),
            ..Default::default()
        };

        let hints = extension_guidance_hints(&comp, Some(ExtensionCapability::Build));

        assert!(hints
            .iter()
            .any(|hint| { hint.contains("homeboy component set plain-package --extension") }));
        assert!(hints
            .iter()
            .any(|hint| { hint.contains("component-level `build_command` is not supported") }));
        assert!(hints
            .iter()
            .any(|hint| hint.contains("homeboy extension list")));
    }

    #[test]
    fn test_should_run() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: Some("test".to_string()),
        };
        assert!(filter.should_run("lint"));
        assert!(!filter.should_run("test"));
        assert!(!filter.should_run("deploy"));
    }

    #[test]
    fn test_to_env_pairs() {
        let filter = RunnerStepFilter {
            step: Some("a".to_string()),
            skip: Some("b".to_string()),
        };
        let env = filter.to_env_pairs();
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_STEP" && v == "a"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "b"));
    }
}
