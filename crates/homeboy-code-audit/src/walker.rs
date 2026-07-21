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
// `walker::is_test_path` / `crate::is_test_path` call sites keep resolving.
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
pub fn is_index_file(path: &Path) -> bool {
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
pub fn walk_source_files_snapshot(root: &Path) -> CodebaseSnapshot {
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

/// Byte ranges `(start, end)` of inline test-region blocks in `content` for a
/// given language, using the language's `inline_test_region_markers()`.
///
/// `is_test_path` only classifies whole test *files*. Detectors that scan raw
/// file content (constant-bypass, command-wrapper-bypass, duplication, …) also
/// need to skip matches inside inline test blocks of production files (Rust's
/// `#[cfg(test)] mod tests { … }`), where literals/commands/fixtures are test
/// scaffolding rather than production code. Languages with no inline test
/// marker (test code lives in separate files) yield no regions.
///
/// For each marker occurrence, brace-matches the block that follows it. Marker
/// occurrences with no following block are ignored. Ranges span marker→closing
/// brace. The concrete marker syntax is sourced from the agnostic language home
/// so this helper carries no hardcoded language token.
pub(crate) fn inline_test_regions(content: &str, markers: &[&str]) -> Vec<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut regions = Vec::new();
    for marker in markers {
        if marker.is_empty() {
            continue;
        }
        let mut search_from = 0usize;
        while let Some(rel) = content[search_from..].find(marker) {
            let marker_start = search_from + rel;
            // Advance past this marker regardless of outcome (no infinite loop).
            search_from = marker_start + marker.len();

            let Some(brace_rel) = content[search_from..].find('{') else {
                continue;
            };
            let open = search_from + brace_rel;
            let mut depth = 0i32;
            let mut close = None;
            for (i, &b) in bytes[open..].iter().enumerate() {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
            }
            if let Some(close) = close {
                regions.push((marker_start, close));
                search_from = close + 1;
            }
        }
    }
    regions.sort_by_key(|(start, _)| *start);
    regions
}

/// Whether `offset` falls within any `(start, end)` region.
pub(crate) fn offset_in_test_region(offset: usize, regions: &[(usize, usize)]) -> bool {
    regions
        .iter()
        .any(|(start, end)| offset >= *start && offset <= *end)
}

/// The crate name for a `crates/<crate>/…` path, else `None` (non-workspace
/// layout — callers fall back to conservative behavior).
///
/// Used by content-scanning detectors that attribute a caller's raw literal or
/// command to a canonical definition elsewhere: a cross-crate attribution is
/// only valid when the definition is reachable from the caller's crate.
pub(crate) fn crate_of_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("crates/")?;
    rest.split('/').next().filter(|s| !s.is_empty())
}

/// Whether a `pub`/`pub(...)` visibility prefix makes a definition reachable
/// from *another* crate. Only crate-wide `pub` qualifies — `pub(crate)`,
/// `pub(super)`, `pub(in …)`, and private are unreachable across a crate
/// boundary, so attributing a cross-crate reference to them is a false positive.
pub(crate) fn visibility_is_crate_public(vis_prefix: &str) -> bool {
    vis_prefix.trim() == "pub"
}

#[cfg(test)]
mod tests {
    use super::{crate_of_path, inline_test_regions, offset_in_test_region};
    use homeboy_engine_primitives::language::Language;

    #[test]
    fn finds_rust_cfg_test_block_via_language_marker() {
        let content = "fn prod() {}\n#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n";
        let regions = inline_test_regions(content, Language::Rust.inline_test_region_markers());
        assert_eq!(regions.len(), 1);
        // The `helper` inside the block is covered; `prod` at the top is not.
        let helper_off = content.find("fn helper").unwrap();
        let prod_off = content.find("fn prod").unwrap();
        assert!(offset_in_test_region(helper_off, &regions));
        assert!(!offset_in_test_region(prod_off, &regions));
    }

    #[test]
    fn no_marker_language_yields_no_regions() {
        // A PHP file with a brace block and no inline marker must yield no
        // regions — the markers slice is empty, so nothing is stripped.
        let content = "<?php\nfunction prod() { return 1; }\n";
        let regions = inline_test_regions(content, Language::Php.inline_test_region_markers());
        assert!(regions.is_empty());
    }

    #[test]
    fn crate_of_path_extracts_workspace_crate() {
        assert_eq!(
            crate_of_path("crates/homeboy-core/src/lib.rs"),
            Some("homeboy-core")
        );
        assert_eq!(crate_of_path("src/lib.rs"), None);
    }
}
