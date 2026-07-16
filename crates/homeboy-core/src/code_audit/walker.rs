//! walker — extracted from conventions.rs.
//!
//! File walking delegated to `homeboy_engine_primitives::codebase_scan` for consistency.

use std::path::Path;

use homeboy_engine_primitives::codebase_scan::{
    self, CodebaseSnapshot, ExtensionFilter, ScanConfig,
};

// `is_test_path` is a pure path classifier consumed by both code_audit and
// extension; it now lives in homeboy-engine-primitives so those two feature
// layers don't have to depend on each other. Re-exported here so the many
// `walker::is_test_path` / `code_audit::is_test_path` call sites keep resolving.
pub use homeboy_engine_primitives::test_path::is_test_path;

/// Extension index/entry-point filenames that should be excluded from convention
/// sibling detection. These files organize other files rather than being
/// peers — including them produces false "missing method" findings.
pub(crate) const INDEX_FILES: &[&str] = &[
    "mod.rs",
    "lib.rs",
    "main.rs",
    "index.js",
    "index.jsx",
    "index.ts",
    "index.tsx",
    "index.mjs",
    "__init__.py",
];

/// Returns true if the filename is a extension index/entry-point file.
pub(crate) fn is_index_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| INDEX_FILES.contains(&name))
        .unwrap_or(false)
}

/// Collect all file extensions that installed extensions can handle.
pub(crate) fn extension_provided_file_extensions() -> Vec<String> {
    super::extension_manifests::load_all_audit_manifests()
        .into_iter()
        .flat_map(|m| m.provided_file_extensions)
        .collect()
}

/// `ScanConfig` matching source snapshots — extension-provided file types only.
fn source_scan_config() -> ScanConfig {
    let dynamic_extensions = extension_provided_file_extensions();
    ScanConfig {
        extensions: ExtensionFilter::Only(dynamic_extensions),
        ..Default::default()
    }
}

fn extension_matches(path: &Path, extensions: &[String]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    extensions.iter().any(|candidate| candidate == ext)
}

pub(crate) fn is_extension_provided_source_file(path: &Path, extensions: &[String]) -> bool {
    extension_matches(path, extensions) && !is_index_file(path)
}

/// Build the shared audit source snapshot for full audit runs.
///
/// The snapshot includes extension-provided convention files plus detector-owned
/// source extensions so downstream detectors can filter one shared walk/read
/// without reducing their existing coverage.
pub(crate) fn walk_shared_audit_files_snapshot(
    root: &Path,
    additional_extensions: &[&str],
) -> CodebaseSnapshot {
    let mut extensions = extension_provided_file_extensions();
    extensions.extend(additional_extensions.iter().map(|ext| (*ext).to_string()));
    extensions.sort();
    extensions.dedup();

    CodebaseSnapshot::build(
        root,
        &ScanConfig {
            extensions: ExtensionFilter::Only(extensions),
            ..Default::default()
        },
    )
}

/// Build a [`CodebaseSnapshot`] of source files under a root.
///
/// Uses extension-provided file types, post-filters extension index files, and
/// reads each file once during the walk and returns owned `(path, content)` pairs ready for
/// downstream consumers (`fingerprint_content`, `FingerprintIndex`, etc.).
///
/// This is the entry point audit consumers should use. Slice 2 of #1492
/// migrates `discovery::auto_discover_groups` and `fingerprint_reference_paths`
/// onto this helper; a later slice moved refactor's `ModuleSurfaceIndex` fallback
/// build path here as well.
pub(crate) fn walk_source_files_snapshot(root: &Path) -> CodebaseSnapshot {
    let mut snapshot = CodebaseSnapshot::build(root, &source_scan_config());
    snapshot.retain(|path| !is_index_file(path));
    snapshot
}

/// Build a snapshot containing every extension-provided source file.
///
/// Convention detectors use [`walk_source_files_snapshot`] to skip index files
/// and later keep only directories with enough siblings. Dead-code analysis uses
/// this broader view as reference-only data so singleton callers, `main.rs`,
/// `lib.rs`, and module facades can still prove exports are live.
pub(crate) fn walk_all_source_files_snapshot(root: &Path) -> CodebaseSnapshot {
    CodebaseSnapshot::build(root, &source_scan_config())
}

/// Known source file extensions that may be present even if no extension claims them.
const COMMON_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "php", "js", "ts", "py", "go", "java", "rb", "swift", "kt", "c", "cpp", "h",
];

/// Count source files that exist in the tree but aren't claimed by any extension.
/// Used to warn when no extension provides fingerprinting for the dominant language.
pub(crate) fn count_unclaimed_source_files(root: &Path) -> usize {
    let claimed = extension_provided_file_extensions();
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(
            COMMON_SOURCE_EXTENSIONS
                .iter()
                .filter(|ext| !claimed.iter().any(|c| c.as_str() == **ext))
                .map(|ext| ext.to_string())
                .collect(),
        ),
        ..Default::default()
    };

    codebase_scan::walk_files(root, &config).len()
}
