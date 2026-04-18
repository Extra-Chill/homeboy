//! Struct field propagation — add missing fields to struct instantiations after
//! a struct definition changes.
//!
//! Scans the codebase for instantiations of a named struct, detects which fields
//! are missing, and inserts them with sensible defaults. Uses the Rust extension's
//! `propagate_struct_fields` refactor script to do the actual analysis.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::extension;
use crate::Error;

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

    // Step 1: Find the struct definition file
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

    // Step 2: Extract the struct source block
    let struct_source = extract_struct_source(struct_name, &def_content).ok_or_else(|| {
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

    // Step 3: Find the extension for .rs files
    let ext_manifest = extension::find_extension_for_file_ext("rs", "refactor").ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            "No extension with refactor capability found for .rs files. Install the Rust extension.",
            None,
            None,
        )
    })?;

    // Step 4: Walk all .rs files using canonical scanner
    let scan_config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
        skip_hidden: true,
        ..Default::default()
    };
    let rs_files = codebase_scan::walk_files(root, &scan_config);

    let def_relative = def_file
        .strip_prefix(root)
        .unwrap_or(&def_file)
        .to_string_lossy()
        .to_string();

    let mut all_edits: Vec<PropagateEdit> = Vec::new();
    let mut total_instantiations = 0usize;
    let mut total_needing_fix = 0usize;
    let mut files_scanned = 0usize;

    crate::log_status!(
        "propagate",
        "Scanning {} .rs files for {} instantiations",
        rs_files.len(),
        struct_name
    );

    for file_path in &rs_files {
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

        let Some(result) = extension::run_refactor_script(&ext_manifest, &cmd) else {
            crate::log_status!("warning", "Extension returned no result for {}", relative);
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

    // Step 5: Apply edits if write mode — route through shared EditOp engine
    let applied = if config.write && !all_edits.is_empty() {
        use crate::engine::edit_op::propagate_result_to_edit_ops;
        use crate::engine::edit_op_apply::apply_edit_ops;

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
        let report = apply_edit_ops(&ops, root).map_err(|e| {
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

/// Find the file containing a struct definition by scanning the codebase.
fn find_struct_definition(struct_name: &str, root: &Path) -> Result<PathBuf, Error> {
    let pattern = format!("pub struct {} ", struct_name);
    let pattern_brace = format!("pub struct {} {{", struct_name);
    let pattern_crate = format!("pub(crate) struct {} ", struct_name);
    let pattern_crate_brace = format!("pub(crate) struct {} {{", struct_name);

    let scan_config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
        skip_hidden: true,
        ..Default::default()
    };
    let files = codebase_scan::walk_files(root, &scan_config);

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };
        if content.contains(&pattern)
            || content.contains(&pattern_brace)
            || content.contains(&pattern_crate)
            || content.contains(&pattern_crate_brace)
        {
            return Ok(file_path.clone());
        }
    }

    Err(Error::validation_invalid_argument(
        "struct_name",
        format!(
            "Could not find struct `{}` in any .rs file under {}",
            struct_name,
            root.display()
        ),
        None,
        Some(vec![format!(
            "homeboy refactor propagate --struct {} --definition src/path/to/file.rs",
            struct_name
        )]),
    ))
}

/// Extract the full struct source block (including doc comments and attributes)
/// from file content.
fn extract_struct_source(struct_name: &str, content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    let struct_pattern = format!("struct {} ", struct_name);
    let struct_pattern_brace = format!("struct {} {{", struct_name);
    let mut start_line = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&struct_pattern) || line.contains(&struct_pattern_brace) {
            // Walk backwards to include attributes and doc comments
            let mut actual_start = i;
            for j in (0..i).rev() {
                let trimmed = lines[j].trim();
                if trimmed.starts_with('#')
                    || trimmed.starts_with("///")
                    || trimmed.starts_with("//!")
                {
                    actual_start = j;
                } else if trimmed.is_empty() {
                    if j > 0
                        && (lines[j - 1].trim().starts_with('#')
                            || lines[j - 1].trim().starts_with("///"))
                    {
                        actual_start = j;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            start_line = Some(actual_start);
            break;
        }
    }

    let start = start_line?;

    // Find the closing brace
    let mut depth = 0i32;
    let mut found_open = false;
    let mut end_line = start;

    for (i, line_content) in lines.iter().enumerate().skip(start) {
        for ch in line_content.chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if found_open && depth == 0 {
            end_line = i;
            break;
        }
    }

    Some(lines[start..=end_line].join("\n"))
}

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
