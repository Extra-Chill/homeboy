//! Struct default collapsing — the inverse of `propagate`.
//!
//! Scans the codebase for instantiations of a named struct and collapses fields
//! whose value equals the type's default into a single trailing
//! `..Default::default()`.
//!
//! # Language dispatch
//!
//! The dispatch is **language-generic**: this module enumerates every installed
//! extension that advertises a `refactor` script, and for each one scans the
//! file extensions it handles and sends candidate files to that extension's
//! `collapse_struct_defaults` command. Any language whose refactor extension
//! implements that command participates automatically — nothing in the loop is
//! Rust-specific.
//!
//! The *analysis* (which values count as a type's default, how a literal is
//! collapsed) lives in the language extension's script, not here. Today only the
//! Rust extension implements `collapse_struct_defaults`, and
//! `..Default::default()` is a Rust idiom — but a Go/Swift/PHP refactor
//! extension that grows the same command is picked up with no change to this
//! file. (`find_struct_definition`/`extract_struct_source` currently parse Rust
//! struct syntax; a non-Rust implementation would supply its own definition
//! source, e.g. via `--definition`.)
//!
//! Dry-run by default: without `write`, it reports what *would* change but
//! touches no files. This lets the generated diffs be validated against
//! known-good hand migrations before `--write` is trusted.

use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy_core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use homeboy_core::Error;
use homeboy_extension as extension;

use crate::propagate::{extract_struct_source, find_struct_definition};

// ============================================================================
// Types
// ============================================================================

/// A single replace-range edit: replace lines `[start_line, end_line]`
/// (1-indexed, inclusive) with `replacement`.
#[derive(Debug, Clone, Serialize)]
pub struct CollapseEdit {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub replacement: String,
    pub description: String,
}

/// Result of a collapse analysis (and optional apply).
#[derive(Debug, Serialize)]
pub struct CollapseResult {
    pub struct_name: String,
    pub definition_file: String,
    pub files_scanned: usize,
    pub instantiations_found: usize,
    pub instantiations_collapsed: usize,
    pub edits: Vec<CollapseEdit>,
    pub applied: bool,
}

/// Configuration for a collapse run.
pub struct CollapseConfig<'a> {
    pub struct_name: &'a str,
    /// Explicit definition file path (auto-detected if `None`).
    pub definition_file: Option<&'a str>,
    pub root: &'a Path,
    pub write: bool,
}

// ============================================================================
// Public API
// ============================================================================

/// Run struct default collapsing: find instantiations whose fields include
/// default-valued entries and collapse them into `..Default::default()`.
pub fn collapse(config: &CollapseConfig) -> Result<CollapseResult, Error> {
    let root = config.root;
    let struct_name = config.struct_name;

    // Step 1: Find the struct definition file.
    let def_file = if let Some(f) = config.definition_file {
        PathBuf::from(f)
    } else {
        find_struct_definition(struct_name, root)?
    };
    let def_path = if def_file.is_absolute() {
        def_file.clone()
    } else {
        root.join(&def_file)
    };
    let def_content = std::fs::read_to_string(&def_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "read struct definition from {}",
                def_path.display()
            )),
        )
    })?;

    // Step 2: Extract the struct source block (field names/types/defaults).
    let struct_source = extract_struct_source(struct_name, &def_content).ok_or_else(|| {
        Error::invalid_argument_for(
            "struct_name",
            format!(
                "Could not find struct `{}` in {}",
                struct_name,
                def_path.display()
            ),
            struct_name,
        )
    })?;

    // Step 3: Discover every refactor-capable extension and the file extensions
    // it handles. The collapse dispatch is language-generic: any language whose
    // refactor extension implements a `collapse_struct_defaults` command
    // participates. The Rust extension is the first implementer, but nothing
    // here is Rust-specific — a Go/Swift/PHP refactor extension that grows the
    // same command is picked up automatically.
    let refactor_exts: Vec<extension::ExtensionManifest> = extension::load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.refactor_script().is_some() && !m.provided_file_extensions().is_empty())
        .collect();

    if refactor_exts.is_empty() {
        return Err(Error::invalid_argument(
            "extension",
            "No extension with refactor capability found. Install a language refactor extension (e.g. the Rust extension).",
        ));
    }

    let def_relative = def_file
        .strip_prefix(root)
        .unwrap_or(&def_file)
        .to_string_lossy()
        .to_string();

    let mut all_edits: Vec<CollapseEdit> = Vec::new();
    let mut total_instantiations = 0usize;
    let mut total_collapsed = 0usize;
    let mut files_scanned = 0usize;

    // Step 4: For each refactor extension, scan the files it handles and send
    // them to that extension's refactor script.
    for ext_manifest in &refactor_exts {
        let handled_exts: Vec<String> = ext_manifest.provided_file_extensions().to_vec();

        let scan_config = ScanConfig {
            extensions: ExtensionFilter::Only(handled_exts.clone()),
            skip_hidden: true,
            ..Default::default()
        };
        let files = codebase_scan::walk_files(root, &scan_config);

        homeboy_core::log_status!(
            "collapse",
            "Scanning {} {} file(s) for {} instantiations",
            files.len(),
            handled_exts.join("/"),
            struct_name
        );

        for file_path in &files {
            let relative = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let Ok(file_content) = std::fs::read_to_string(file_path) else {
                continue;
            };

            // Quick check: skip files that don't mention the struct name.
            if !file_content.contains(struct_name) {
                continue;
            }
            files_scanned += 1;

            let cmd = serde_json::json!({
                "command": "collapse_struct_defaults",
                "struct_name": struct_name,
                "struct_source": struct_source,
                "file_content": file_content,
                "file_path": relative,
            });

            let Some(result) = extension::run_refactor_script(ext_manifest, &cmd) else {
                homeboy_core::log_status!(
                    "warning",
                    "Extension returned no result for {}",
                    relative
                );
                continue;
            };

            if let Some(found) = result.get("instantiations_found").and_then(|v| v.as_u64()) {
                total_instantiations += found as usize;
            }
            if let Some(collapsed) = result
                .get("instantiations_collapsed")
                .and_then(|v| v.as_u64())
            {
                total_collapsed += collapsed as usize;
            }

            if let Some(edits) = result.get("edits").and_then(|v| v.as_array()) {
                for edit in edits {
                    let file = edit
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&relative)
                        .to_string();
                    let start_line =
                        edit.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let end_line =
                        edit.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let replacement = edit
                        .get("replacement")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = edit
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    if start_line == 0 || end_line < start_line {
                        continue;
                    }

                    all_edits.push(CollapseEdit {
                        file,
                        start_line,
                        end_line,
                        replacement,
                        description,
                    });
                }
            }
        }
    }

    // Step 5: Apply edits if write mode. Apply per file, bottom-up, so earlier
    // line numbers stay valid as ranges are spliced.
    let applied = if config.write && !all_edits.is_empty() {
        apply_collapse_edits(root, &all_edits)?;
        true
    } else {
        false
    };

    Ok(CollapseResult {
        struct_name: struct_name.to_string(),
        definition_file: def_relative,
        files_scanned,
        instantiations_found: total_instantiations,
        instantiations_collapsed: total_collapsed,
        edits: all_edits,
        applied,
    })
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Apply replace-range edits to files on disk. Groups edits by file and applies
/// each file's edits from the bottom up so line numbers stay valid.
fn apply_collapse_edits(root: &Path, edits: &[CollapseEdit]) -> Result<(), Error> {
    use std::collections::BTreeMap;

    let mut by_file: BTreeMap<&str, Vec<&CollapseEdit>> = BTreeMap::new();
    for edit in edits {
        by_file.entry(edit.file.as_str()).or_default().push(edit);
    }

    for (file, mut file_edits) in by_file {
        let path = root.join(file);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("read {}", path.display())))
        })?;
        let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();

        // Bottom-up so splices don't shift not-yet-applied ranges.
        file_edits.sort_by(|a, b| b.start_line.cmp(&a.start_line));

        for edit in file_edits {
            // 1-indexed inclusive → 0-indexed slice range.
            let start = edit.start_line.saturating_sub(1);
            let end = edit.end_line; // slice end is exclusive == inclusive last line
            if start >= lines.len() || end > lines.len() || start >= end {
                continue;
            }
            let replacement: Vec<String> = edit
                .replacement
                .split('\n')
                .map(|s| s.to_string())
                .collect();
            lines.splice(start..end, replacement);
        }

        let new_content = lines.join("\n");
        std::fs::write(&path, new_content).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_temp(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let rel = "src/example.rs";
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        fs::write(&path, content).expect("write");
        (dir, rel.to_string())
    }

    #[test]
    fn apply_collapse_splices_range_and_reduces_lines() {
        let content = "\
let x = Foo {
    a: 1,
    b: None,
    c: None,
};
";
        let (dir, rel) = write_temp(content);
        // Replace inner lines 2..=4 (a/b/c) with the kept `a: 1,` + spread.
        let edits = vec![CollapseEdit {
            file: rel.clone(),
            start_line: 2,
            end_line: 4,
            replacement: "    a: 1,\n    ..Default::default()".to_string(),
            description: "test".to_string(),
        }];
        apply_collapse_edits(dir.path(), &edits).expect("apply");

        let result = fs::read_to_string(dir.path().join(&rel)).expect("read");
        assert!(result.contains("a: 1,"));
        assert!(result.contains("..Default::default()"));
        assert!(!result.contains("b: None"));
        assert!(!result.contains("c: None"));
        // Braces stay balanced.
        assert_eq!(
            result.matches('{').count(),
            result.matches('}').count(),
            "braces must stay balanced"
        );
    }

    #[test]
    fn apply_collapse_applies_multiple_edits_bottom_up() {
        let content = "\
let a = Foo {
    x: 1,
    y: None,
};
let b = Foo {
    x: 2,
    y: None,
};
";
        let (dir, rel) = write_temp(content);
        let edits = vec![
            CollapseEdit {
                file: rel.clone(),
                start_line: 2,
                end_line: 3,
                replacement: "    x: 1,\n    ..Default::default()".to_string(),
                description: "first".to_string(),
            },
            CollapseEdit {
                file: rel.clone(),
                start_line: 6,
                end_line: 7,
                replacement: "    x: 2,\n    ..Default::default()".to_string(),
                description: "second".to_string(),
            },
        ];
        apply_collapse_edits(dir.path(), &edits).expect("apply");

        let result = fs::read_to_string(dir.path().join(&rel)).expect("read");
        assert!(result.contains("x: 1,"));
        assert!(result.contains("x: 2,"));
        assert_eq!(result.matches("..Default::default()").count(), 2);
        assert!(!result.contains("y: None"));
    }
}
