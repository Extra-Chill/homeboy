//! Struct field propagation — add missing fields to struct instantiations after
//! a struct definition changes.
//!
//! Scans the codebase for instantiations of a named struct, detects which fields
//! are missing, and inserts them with sensible defaults.
//!
//! # Language dispatch
//!
//! The dispatch is **language-generic**: this module enumerates every installed
//! extension that advertises a `refactor` script and, for each, scans the file
//! extensions it handles and sends candidate files to that extension's
//! `propagate_struct_fields` command. Any language whose refactor extension
//! implements that command participates automatically — nothing in the scan
//! loop is Rust-specific. Definition-finding and source-block extraction are
//! also delegated to the language extension (via the `find_definition` command
//! in [`crate::definition`]), so no struct syntax is parsed in this crate. Today
//! only the Rust extension implements these commands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy_core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use homeboy_core::Error;
use homeboy_extension as extension;

// ============================================================================
// Types
// ============================================================================

/// A struct field discovered during propagation.
#[derive(Debug, Clone, Serialize)]
pub struct PropagateField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub default: String,
}

/// A single edit to insert a missing field at a specific line.
#[derive(Debug, Clone, Serialize)]
pub struct PropagateEdit {
    pub file: String,
    pub line: usize,
    pub insert_text: String,
    pub description: String,
}

/// Result of a propagation analysis.
#[derive(Debug, Serialize)]
pub struct PropagateResult {
    pub struct_name: String,
    pub definition_file: String,
    pub fields: Vec<PropagateField>,
    pub files_scanned: usize,
    pub instantiations_found: usize,
    pub instantiations_needing_fix: usize,
    pub edits: Vec<PropagateEdit>,
    pub applied: bool,
}

// ============================================================================
// Public API
// ============================================================================

/// Configuration for a propagation run.
pub struct PropagateConfig<'a> {
    pub struct_name: &'a str,
    /// Explicit definition file path (auto-detected if `None`).
    pub definition_file: Option<&'a str>,
    pub root: &'a Path,
    pub write: bool,
}

/// Run struct field propagation: find instantiations with missing fields and
/// optionally insert defaults.
pub fn propagate(config: &PropagateConfig) -> Result<PropagateResult, Error> {
    let root = config.root;
    let struct_name = config.struct_name;

    // Discover every refactor-capable extension. Propagation is language-generic:
    // any language whose refactor extension implements `propagate_struct_fields`
    // (and `find_definition`) participates. Nothing below is Rust-specific — the
    // Rust extension is merely the first implementer. Definition-finding and
    // source extraction are delegated to the language extension via the
    // `find_definition` command, so no struct syntax is parsed in this crate.
    let refactor_exts = crate::definition::refactor_capable_extensions()?;

    // Step 1 & 2: Locate the struct definition and extract its source block.
    // When `--definition` is supplied the file is known but the source-block
    // extraction is still language-specific, so it goes through the extension too.
    let (def_file, struct_source) = if let Some(f) = config.definition_file {
        let def_file = PathBuf::from(f);
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
        let relative = def_file
            .strip_prefix(root)
            .unwrap_or(&def_file)
            .to_string_lossy()
            .to_string();
        let source = crate::definition::extract_definition_source(
            struct_name,
            &def_content,
            &relative,
            &refactor_exts,
        )
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "struct_name",
                format!(
                    "Could not find struct `{}` in {}",
                    struct_name,
                    def_path.display()
                ),
                None,
                None,
            )
        })?;
        (def_file, source)
    } else {
        let located = crate::definition::find_definition(struct_name, root, &refactor_exts)?;
        (located.file, located.source)
    };

    let def_relative = def_file
        .strip_prefix(root)
        .unwrap_or(&def_file)
        .to_string_lossy()
        .to_string();

    let mut all_edits: Vec<PropagateEdit> = Vec::new();
    let mut total_instantiations = 0usize;
    let mut total_needing_fix = 0usize;
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
            "propagate",
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

            // Quick check: skip files that don't mention the struct name
            if !file_content.contains(struct_name) {
                continue;
            }

            files_scanned += 1;

            let cmd = serde_json::json!({
                "command": "propagate_struct_fields",
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
            if let Some(needing) = result
                .get("instantiations_needing_fix")
                .and_then(|v| v.as_u64())
            {
                total_needing_fix += needing as usize;
            }

            if let Some(edits) = result.get("edits").and_then(|v| v.as_array()) {
                for edit in edits {
                    let file = edit
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&relative)
                        .to_string();
                    let line = edit.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let insert_text = edit
                        .get("insert_text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = edit
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    all_edits.push(PropagateEdit {
                        file,
                        line,
                        insert_text,
                        description,
                    });
                }
            }
        }
    }

    // Step 5: Apply edits if write mode — route through shared EditOp engine
    let applied = if config.write && !all_edits.is_empty() {
        use crate::edit_op_tagged::propagate_result_to_edit_ops;
        use homeboy_core::engine::edit_op_apply::apply_edit_ops;

        // Build a temporary PropagateResult to convert edits
        let tmp_result = PropagateResult {
            struct_name: struct_name.to_string(),
            definition_file: String::new(),
            fields: vec![],
            files_scanned: 0,
            instantiations_found: 0,
            instantiations_needing_fix: 0,
            edits: all_edits.clone(),
            applied: false,
        };
        let ops = propagate_result_to_edit_ops(&tmp_result);
        let plain_ops: Vec<_> = ops.iter().map(|t| t.op.clone()).collect();
        let report = apply_edit_ops(&plain_ops, root).map_err(|e| {
            Error::internal_io(e.to_string(), Some("apply propagate edits".to_string()))
        })?;
        report.files_modified > 0 || report.ops_applied > 0
    } else {
        false
    };

    // Extract field info from collected edits
    let fields = extract_fields_from_edits(&all_edits);

    Ok(PropagateResult {
        struct_name: struct_name.to_string(),
        definition_file: def_relative,
        fields,
        files_scanned,
        instantiations_found: total_instantiations,
        instantiations_needing_fix: total_needing_fix,
        edits: all_edits,
        applied,
    })
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Extract field information from propagation edits.
///
/// Each edit's `description` contains the field name (between backticks) and the
/// `insert_text` contains the default value (after the colon).
fn extract_fields_from_edits(edits: &[PropagateEdit]) -> Vec<PropagateField> {
    let mut seen = HashSet::new();
    edits
        .iter()
        .filter_map(|e| {
            let start = e.description.find('`')? + 1;
            let end = e.description[start..].find('`')? + start;
            let field_name = &e.description[start..end];
            if seen.insert(field_name.to_string()) {
                let trimmed = e.insert_text.trim().trim_end_matches(',');
                let colon_pos = trimmed.find(':')?;
                let default = trimmed[colon_pos + 1..].trim().to_string();
                Some(PropagateField {
                    name: field_name.to_string(),
                    field_type: String::new(),
                    default,
                })
            } else {
                None
            }
        })
        .collect()
}
