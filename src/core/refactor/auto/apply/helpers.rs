//! helpers — extracted from apply.rs.

use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use regex::Regex;
use crate::code_audit::conventions::Language;
use crate::engine::undo::InMemoryRollback;
use std::path::Path;


/// Remove a function name from `pub use { ... }` blocks.
///
/// Handles both single-line (`pub use module::{a, b, c};`) and multi-line
/// re-export blocks. Removes the name and trailing comma. If the block
/// becomes empty after removal, removes the entire `pub use` statement.
pub(crate) fn remove_from_pub_use_block(lines: &mut Vec<String>, fn_name: &str) {
    let word_pattern = format!(r"\b{}\b", regex::escape(fn_name));
    let word_re = match regex::Regex::new(&word_pattern) {
        Ok(re) => re,
        Err(_) => return,
    };

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim().to_string();

        // Single-line: pub use module::{a, b, c};
        if trimmed.starts_with("pub use") && trimmed.contains('{') && trimmed.contains('}') {
            if word_re.is_match(&trimmed) {
                // Remove the name (and surrounding comma/whitespace)
                let cleaned = word_re
                    .replace(&lines[i], "")
                    .to_string()
                    .replace(", ,", ",")
                    .replace("{, ", "{ ")
                    .replace("{,", "{")
                    .replace(", }", " }")
                    .replace(",}", "}");

                // Check if the block is now empty
                if let Some(start) = cleaned.find('{') {
                    if let Some(end) = cleaned.find('}') {
                        let inside = cleaned[start + 1..end].trim();
                        if inside.is_empty() {
                            lines.remove(i);
                            continue;
                        }
                    }
                }
                lines[i] = cleaned;
            }
            i += 1;
            continue;
        }

        // Multi-line block: pub use module::{
        if trimmed.starts_with("pub use") && trimmed.contains('{') && !trimmed.contains('}') {
            let block_start = i;
            i += 1;
            while i < lines.len() {
                let inner = lines[i].trim().to_string();
                if word_re.is_match(&inner) {
                    // Remove the matched name (and surrounding comma/whitespace)
                    let cleaned = word_re
                        .replace(&inner, "")
                        .to_string()
                        .replace(", ,", ",")
                        .trim()
                        .to_string();
                    // Strip leading/trailing commas to normalize
                    let cleaned = cleaned
                        .trim_start_matches(',')
                        .trim_end_matches(',')
                        .trim()
                        .to_string();
                    if cleaned.is_empty() {
                        lines.remove(i);
                        continue;
                    }
                    // Restore trailing comma for non-closing lines in a multi-line block.
                    // Without this, removing an item from the end of a line leaves the
                    // previous items without a trailing comma, breaking the next line.
                    let needs_trailing_comma = !cleaned.contains('}');
                    let final_cleaned = if needs_trailing_comma && !cleaned.ends_with(',') {
                        format!("{},", cleaned)
                    } else {
                        cleaned
                    };
                    let indent = " ".repeat(lines[i].len() - lines[i].trim_start().len());
                    lines[i] = format!("{}{}", indent, final_cleaned);
                }
                if lines[i].trim().contains('}') {
                    break;
                }
                i += 1;
            }

            // Check if the block is now empty (only opening and closing lines remain)
            let block_end = i.min(lines.len() - 1);
            let has_items = (block_start + 1..block_end)
                .any(|j| !lines[j].trim().is_empty() && lines[j].trim() != ",");
            if !has_items {
                // Remove entire block
                for _ in block_start..=block_end.min(lines.len() - 1) {
                    if block_start < lines.len() {
                        lines.remove(block_start);
                    }
                }
                i = block_start;
                continue;
            }
        }

        i += 1;
    }
}

pub fn auto_apply_subset(result: &FixResult) -> FixResult {
    let fixes: Vec<Fix> = result
        .fixes
        .iter()
        .filter_map(|fix| {
            let insertions: Vec<Insertion> = fix
                .insertions
                .iter()
                .filter(|insertion| insertion.auto_apply)
                .cloned()
                .collect();

            if insertions.is_empty() {
                None
            } else {
                Some(Fix {
                    file: fix.file.clone(),
                    required_methods: fix.required_methods.clone(),
                    required_registrations: fix.required_registrations.clone(),
                    insertions,
                    applied: false,
                })
            }
        })
        .collect();

    let new_files: Vec<NewFile> = result
        .new_files
        .iter()
        .filter(|new_file| new_file.auto_apply)
        .cloned()
        .collect();

    let decompose_plans = result.decompose_plans.clone();

    let total_insertions =
        fixes.iter().map(|fix| fix.insertions.len()).sum::<usize>() + new_files.len();

    FixResult {
        fixes,
        new_files,
        decompose_plans,
        skipped: vec![],
        chunk_results: vec![],
        total_insertions,
        files_modified: 0,
    }
}
