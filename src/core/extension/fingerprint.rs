use super::manifest::ExtensionManifest;
use crate::core::engine::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};

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
///   "imports": ["crate::core::error::Result"]
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
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES).ok()
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

/// Extension-reported direct aggregate literal construction site.
///
/// Language extensions own syntax recognition; core only groups these generic
/// facts to detect repeated inline construction where a canonical seam exists.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AggregateLiteral {
    /// Aggregate type name, e.g. a struct/class/interface-backed record.
    pub type_name: String,
    /// Field/property names initialized at the literal site.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Source line where the literal starts, if available.
    #[serde(default)]
    pub line: usize,
}

/// Extension-reported canonical construction seam for an aggregate type.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AggregateConstructionSeam {
    /// Aggregate type name this seam constructs.
    pub type_name: String,
    /// Generic seam name, e.g. `new`, `builder`, `from_config`, `build_foo`.
    pub method: String,
    /// Source line where the seam is declared, if available.
    #[serde(default)]
    pub line: usize,
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
    /// Type/class names registered with a runtime dispatcher.
    ///
    /// Framework-specific extension scripts populate this for patterns where a
    /// type is registered in one file and its public methods are invoked by the
    /// runtime in another file.
    #[serde(default)]
    pub runtime_dispatched_types: Vec<String>,
    /// Opaque extension-provided tags used to keep convention inference from
    /// mixing unlike source roles. Core never interprets tag values; it only
    /// groups files with the same normalized tag set together.
    #[serde(default)]
    pub convention_tags: Vec<String>,
    /// Direct aggregate literal construction sites reported by an extension.
    #[serde(default)]
    pub aggregate_literals: Vec<AggregateLiteral>,
    /// Canonical construction seams reported by an extension.
    #[serde(default)]
    pub aggregate_construction_seams: Vec<AggregateConstructionSeam>,
}
