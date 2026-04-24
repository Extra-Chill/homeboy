//! fingerprint — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;

use super::conventions::Language;

/// A structural fingerprint extracted from a single source file.
#[derive(Debug, Clone, Default)]
pub struct FileFingerprint {
    /// Path relative to component root.
    pub relative_path: String,
    /// Language detected from extension.
    pub language: Language,
    /// Method/function names found in the file.
    ///
    /// Excludes inline test functions — those that have a `#[test]` (or
    /// framework-specific test) attribute are tracked separately in
    /// `test_methods`. A production method whose name happens to start with
    /// a test-convention prefix (e.g. `ExtensionManifest::test_script()`)
    /// still lives here, where it belongs.
    pub methods: Vec<String>,
    /// Inline test method names, prefixed per `TestMappingConfig.method_prefix`.
    ///
    /// Populated ONLY for functions with an explicit test attribute (e.g.
    /// `#[test]` in Rust) when the core grammar engine fingerprints the file.
    /// Extension-script fingerprinting (PHP/JS/TS) leaves this empty because
    /// those languages have no structural test marker — callers that care
    /// about the non-inline case fall back to filtering `methods` by prefix.
    ///
    /// This exists so a production method named `test_foo()` is never
    /// confused with an inline `#[test] fn test_foo()`. Without this field
    /// the orphaned-test detector emitted source-file findings that
    /// downstream autofix treated as deletable test functions — see
    /// Extra-Chill/homeboy#1471.
    pub test_methods: Vec<String>,
    /// Registration calls found (e.g., add_action, register_rest_route).
    pub registrations: Vec<String>,
    /// Class or struct name if found.
    pub type_name: Option<String>,
    /// All public type names found in the file.
    pub type_names: Vec<String>,
    /// Parent class name (e.g., "WC_Abstract_Order").
    pub extends: Option<String>,
    /// Interfaces or traits implemented.
    pub implements: Vec<String>,
    /// Namespace declaration (PHP namespace, Rust mod path).
    pub namespace: Option<String>,
    /// Import/use statements.
    pub imports: Vec<String>,
    /// Raw file content (for import usage analysis).
    pub content: String,
    /// Method name → normalized body hash for duplication detection.
    /// Populated by extension scripts that support it; empty otherwise.
    pub method_hashes: HashMap<String, String>,
    /// Method name → structural hash for near-duplicate detection.
    /// Identifiers/literals replaced with positional tokens before hashing.
    /// Populated by extension scripts that support it; empty otherwise.
    pub structural_hashes: HashMap<String, String>,
    /// Method name → visibility ("public", "protected", "private").
    pub visibility: HashMap<String, String>,
    /// Public/protected class properties (e.g., ["string $name", "$data"]).
    pub properties: Vec<String>,
    /// Hook references: do_action() and apply_filters() calls.
    pub hooks: Vec<crate::extension::HookRef>,
    /// Function parameters that are declared but never used in the function body.
    pub unused_parameters: Vec<crate::extension::UnusedParam>,
    /// Dead code suppression markers (e.g., `#[allow(dead_code)]`).
    pub dead_code_markers: Vec<crate::extension::DeadCodeMarker>,
    /// Function/method names called within this file.
    pub internal_calls: Vec<String>,
    /// Call sites with argument counts (for cross-file parameter analysis).
    pub call_sites: Vec<crate::extension::CallSite>,
    /// Public functions/methods exported from this file.
    pub public_api: Vec<String>,
    /// Functions/methods registered as hook/callback targets from WITHIN
    /// this file (populated by extension fingerprint scripts). Used by the
    /// dead-code check to recognize that a function defined and
    /// hook-registered in the same file IS live code — invoked by the
    /// framework runtime rather than direct calls from other files.
    pub hook_callbacks: Vec<String>,
    /// Method names that are trait implementations (called via trait dispatch).
    pub trait_impl_methods: Vec<String>,
}

/// Extract a structural fingerprint from a source file.
///
/// Tries the grammar-driven core engine first (no subprocess, faster, testable).
/// Falls back to the extension fingerprint script if no grammar is available
/// or the core engine can't handle the file.
pub fn fingerprint_file(path: &Path, root: &Path) -> Option<FileFingerprint> {
    let ext = path.extension()?.to_str()?;
    let content = std::fs::read_to_string(path).ok()?;
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    // Try core grammar engine first
    if let Some(grammar) = super::core_fingerprint::load_grammar_for_ext(ext) {
        if let Some(fp) =
            super::core_fingerprint::fingerprint_from_grammar(&content, &grammar, &relative_path)
        {
            return Some(fp);
        }
    }

    // Fall back to extension fingerprint script
    fingerprint_via_extension(ext, &content, &relative_path)
}

/// Fingerprint using the extension script protocol (legacy path).
fn fingerprint_via_extension(
    ext: &str,
    content: &str,
    relative_path: &str,
) -> Option<FileFingerprint> {
    use crate::extension;

    let matched_extension = extension::find_extension_for_file_ext(ext, "fingerprint")?;
    let output = extension::run_fingerprint_script(&matched_extension, relative_path, content)?;

    let language = Language::from_extension(ext);

    Some(FileFingerprint {
        relative_path: relative_path.to_string(),
        language,
        methods: output.methods,
        // Extension-script fingerprinting does not distinguish test methods
        // structurally — callers fall back to prefix-filter on `methods` when
        // `test_methods` is empty and `inline_tests` is false.
        test_methods: Vec::new(),
        registrations: output.registrations,
        type_name: output.type_name,
        type_names: output.type_names,
        extends: output.extends,
        implements: output.implements,
        namespace: output.namespace,
        imports: output.imports,
        content: content.to_string(),
        method_hashes: output.method_hashes,
        structural_hashes: output.structural_hashes,
        visibility: output.visibility,
        properties: output.properties,
        hooks: output.hooks,
        unused_parameters: output.unused_parameters,
        dead_code_markers: output.dead_code_markers,
        internal_calls: output.internal_calls,
        call_sites: output.call_sites,
        public_api: output.public_api,
        hook_callbacks: output.hook_callbacks,
        trait_impl_methods: Vec::new(), // Extension scripts don't track this
    })
}
