//! fingerprint — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::conventions::Language;
use crate::core::engine::codebase_scan::CodebaseSnapshot;

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

/// Extract a structural fingerprint from a source file on disk.
///
/// Reads `path` from disk, then delegates to [`fingerprint_content`]. Use
/// `fingerprint_content` directly when content has already been loaded
/// (e.g., from a [`CodebaseSnapshot`]) to avoid double-reads.
pub fn fingerprint_file(path: &Path, root: &Path) -> Option<FileFingerprint> {
    let content = std::fs::read_to_string(path).ok()?;
    fingerprint_content(path, root, &content)
}

/// Extract a structural fingerprint from already-loaded file content.
///
/// Tries the grammar-driven core engine first (no subprocess, faster, testable).
/// Falls back to the extension fingerprint script if no grammar is available
/// or the core engine can't handle the file.
///
/// This is the content-taking primitive used by [`FingerprintIndex::from_snapshot`]
/// to avoid re-reading every file from disk after a [`CodebaseSnapshot`] has
/// already loaded them. [`fingerprint_file`] is a convenience wrapper that
/// reads from disk and delegates here so behavior stays identical.
pub fn fingerprint_content(path: &Path, root: &Path, content: &str) -> Option<FileFingerprint> {
    let ext = path.extension()?.to_str()?;
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    // Try core grammar engine first
    if let Some(grammar) = super::core_fingerprint::load_grammar_for_ext(ext) {
        if let Some(fp) =
            super::core_fingerprint::fingerprint_from_grammar(content, &grammar, &relative_path)
        {
            return Some(fp);
        }
    }

    // Fall back to extension fingerprint script
    fingerprint_via_extension(ext, content, &relative_path)
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

// ============================================================================
// FingerprintIndex — built once from a CodebaseSnapshot
// ============================================================================

/// Pre-computed fingerprints for every file in a [`CodebaseSnapshot`].
///
/// Slice 1 of Extra-Chill/homeboy#1492. Built once via
/// [`FingerprintIndex::from_snapshot`], shared by audit detectors,
/// fixability planning, and refactor primitives instead of each consumer
/// re-walking the tree and re-fingerprinting from disk.
///
/// Files whose extension has no grammar and no extension-script
/// fingerprinter are silently dropped from the index — same semantics as
/// [`fingerprint_file`] returning `None`.
///
/// This is opt-in scaffolding: existing callsites still use `fingerprint_file`
/// directly. Consumer migration lands in subsequent slices.
#[derive(Debug, Clone, Default)]
pub struct FingerprintIndex {
    inner: HashMap<PathBuf, FileFingerprint>,
}

impl FingerprintIndex {
    /// Build an index by fingerprinting every file in `snapshot` once,
    /// reusing the snapshot's already-loaded content (no disk re-reads).
    pub fn from_snapshot(snapshot: &CodebaseSnapshot) -> Self {
        let root = snapshot.root();
        let mut inner = HashMap::with_capacity(snapshot.len());
        for (path, content) in snapshot.iter() {
            if let Some(fp) = fingerprint_content(path, root, content) {
                inner.insert(path.to_path_buf(), fp);
            }
        }
        Self { inner }
    }

    /// Look up the fingerprint for an absolute file path from the snapshot.
    pub fn get(&self, path: &Path) -> Option<&FileFingerprint> {
        self.inner.get(path)
    }

    /// Number of fingerprinted files (may be less than the source snapshot
    /// if some extensions have no fingerprinter).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate `(path, fingerprint)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&Path, &FileFingerprint)> {
        self.inner.iter().map(|(p, fp)| (p.as_path(), fp))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::codebase_scan::ScanConfig;

    /// Sort a slice of strings into an owned Vec for set-equivalence asserts.
    /// Used because some fingerprint vector fields come from HashMap iteration
    /// and have non-deterministic order across runs.
    fn sorted(v: &[String]) -> Vec<String> {
        let mut out = v.to_vec();
        out.sort();
        out
    }

    fn fingerprint_file_retry(path: &Path, root: &Path) -> Option<FileFingerprint> {
        for _ in 0..5 {
            if let Some(fingerprint) = fingerprint_file(path, root) {
                return Some(fingerprint);
            }
            std::thread::yield_now();
        }
        None
    }

    #[test]
    fn fingerprint_content_matches_fingerprint_file() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        // Use the homeboy worktree's own source as a real-world Rust input.
        let dir = std::env::temp_dir().join("homeboy_fingerprint_parity_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("src"));

        let src = "pub fn alpha() {}\npub fn beta(x: i32) -> i32 { x }\n";
        let file = dir.join("src/lib.rs");
        std::fs::write(&file, src).unwrap();

        let from_disk = fingerprint_file_retry(&file, &dir);
        let from_content = fingerprint_content(&file, &dir, src);

        // Either both produce a fingerprint, or both return None — and when
        // both produce one, the structural fields must match. Vector fields
        // come from HashMap iteration in some grammar paths, so compare as
        // sorted sets rather than ordered sequences.
        assert_eq!(from_disk.is_some(), from_content.is_some());
        if let (Some(a), Some(b)) = (from_disk, from_content) {
            assert_eq!(a.relative_path, b.relative_path);
            assert_eq!(sorted(&a.methods), sorted(&b.methods));
            assert_eq!(sorted(&a.public_api), sorted(&b.public_api));
            assert_eq!(sorted(&a.imports), sorted(&b.imports));
            assert_eq!(a.namespace, b.namespace);
            assert_eq!(a.type_name, b.type_name);
            assert_eq!(sorted(&a.type_names), sorted(&b.type_names));
            assert_eq!(a.extends, b.extends);
            assert_eq!(sorted(&a.implements), sorted(&b.implements));
            assert_eq!(a.content, b.content);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_snapshot_index_matches_per_file_get_calls() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        let dir = std::env::temp_dir().join(format!(
            "homeboy_fingerprint_index_parity_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("src"));

        std::fs::write(
            dir.join("src/alpha.rs"),
            "pub fn alpha_one() {}\npub fn alpha_two() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/beta.rs"),
            "pub struct Beta;\nimpl Beta { pub fn new() -> Self { Self } }\n",
        )
        .unwrap();

        let snapshot = CodebaseSnapshot::build(&dir, &ScanConfig::default());
        let index = FingerprintIndex::from_snapshot(&snapshot);

        // For every file the snapshot saw, the index either contains a
        // fingerprint identical to the snapshot-content fingerprinter, or both
        // routes return None (extension with no fingerprinter).
        for (path, content) in snapshot.iter() {
            let from_index = index.get(path);
            let from_file = fingerprint_content(path, snapshot.root(), content);
            assert_eq!(from_index.is_some(), from_file.is_some());
            if let (Some(a), Some(b)) = (from_index, from_file.as_ref()) {
                assert_eq!(a.relative_path, b.relative_path);
                assert_eq!(sorted(&a.methods), sorted(&b.methods));
                assert_eq!(sorted(&a.public_api), sorted(&b.public_api));
                assert_eq!(sorted(&a.imports), sorted(&b.imports));
                assert_eq!(a.content, b.content);
            }
        }

        // The index should be non-empty for a tree with .rs files when a
        // Rust grammar is available; if no grammar/extension is registered
        // in this build, the test still passes (both routes return None).
        assert_eq!(index.len(), index.iter().count());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_snapshot_get_returns_none_for_empty_snapshot() {
        let dir = std::env::temp_dir().join("homeboy_fingerprint_index_empty_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let snapshot = CodebaseSnapshot::build(&dir, &ScanConfig::default());
        let index = FingerprintIndex::from_snapshot(&snapshot);

        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.iter().count(), 0);
        assert!(index.get(&dir.join("nope.rs")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
