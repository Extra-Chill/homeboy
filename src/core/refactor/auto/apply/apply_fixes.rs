//! apply_fixes — extracted from apply.rs.

use crate::code_audit::conventions::Language;
use crate::core::refactor::plan::verify::rewrite_callers_after_dedup;
use crate::engine::undo::InMemoryRollback;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use std::path::Path;
use regex::Regex;


pub fn apply_fixes(fixes: &mut [Fix], root: &Path) -> usize {
    apply_fixes_chunked(fixes, root, ApplyOptions { verifier: None })
        .iter()
        .filter(|chunk| matches!(chunk.status, ChunkStatus::Applied))
        .map(|chunk| chunk.applied_files)
        .sum()
}

/// Merge insertions from fixes targeting the same file into the first fix for
/// that file. This ensures all `FunctionRemoval` line ranges are applied against
/// the original file content in a single pass. Without this, the second fix
/// re-reads the already-modified file but uses line numbers from the original,
/// causing brace corruption when lines shift after the first removal.
pub(crate) fn merge_same_file_insertions(fixes: &mut [Fix]) {
    use std::collections::HashMap;

    // Map file path → index of the first fix for that file
    let mut first_for_file: HashMap<&str, usize> = HashMap::new();
    let mut merge_sources: Vec<(usize, usize)> = Vec::new(); // (donor_idx, target_idx)

    for (i, fix) in fixes.iter().enumerate() {
        match first_for_file.get(fix.file.as_str()) {
            Some(&target_idx) => {
                merge_sources.push((i, target_idx));
            }
            None => {
                first_for_file.insert(&fix.file, i);
            }
        }
    }

    // Move insertions from donor fixes into the target (first) fix for each file
    for (donor_idx, target_idx) in merge_sources {
        // Split the slice to get mutable references to both elements
        if donor_idx > target_idx {
            let (left, right) = fixes.split_at_mut(donor_idx);
            left[target_idx]
                .insertions
                .extend(right[0].insertions.drain(..));
        } else {
            let (left, right) = fixes.split_at_mut(target_idx);
            right[0]
                .insertions
                .extend(left[donor_idx].insertions.drain(..));
        }
    }
}

pub fn apply_fixes_chunked(
    fixes: &mut [Fix],
    root: &Path,
    options: ApplyOptions<'_>,
) -> Vec<ApplyChunkResult> {
    let mut results = Vec::new();

    // Merge fixes targeting the same file so all insertions (especially
    // FunctionRemoval line ranges) are applied to the original content in a
    // single pass. Without this, the second fix re-reads the already-modified
    // file but uses line numbers from the original, causing brace corruption.
    merge_same_file_insertions(fixes);

    for (index, fix) in fixes.iter_mut().enumerate() {
        let abs_path = root.join(&fix.file);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                results.push(ApplyChunkResult {
                    chunk_id: format!("fix:{}", index + 1),
                    files: vec![fix.file.clone()],
                    status: ChunkStatus::Reverted,
                    applied_files: 0,
                    reverted_files: 0,
                    verification: None,
                    error: Some(format!("Failed to read {}: {}", fix.file, e)),
                });
                continue;
            }
        };

        let language: Language = Language::from_path(&abs_path);
        let modified = apply_insertions_to_content(&content, &fix.insertions, &language);

        if modified == content {
            results.push(ApplyChunkResult {
                chunk_id: format!("fix:{}", index + 1),
                files: vec![fix.file.clone()],
                status: ChunkStatus::Applied,
                applied_files: 0,
                reverted_files: 0,
                verification: Some("no_op".to_string()),
                error: None,
            });
            continue;
        }

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&abs_path);

        match std::fs::write(&abs_path, &modified) {
            Ok(_) => {
                // Format the written file before verification so lint smoke
                // checks pass on generated code (e.g., test modules).
                let _ = crate::engine::format_write::format_after_write(root, &[abs_path.clone()]);

                let mut chunk = ApplyChunkResult {
                    chunk_id: format!("fix:{}", index + 1),
                    files: vec![fix.file.clone()],
                    status: ChunkStatus::Applied,
                    applied_files: 1,
                    reverted_files: 0,
                    verification: Some("write_ok".to_string()),
                    error: None,
                };

                if let Some(verifier) = options.verifier {
                    match verifier(&chunk) {
                        Ok(verification) => {
                            chunk.verification = Some(verification);
                        }
                        Err(error) => {
                            rollback.restore_all();
                            chunk.status = ChunkStatus::Reverted;
                            chunk.reverted_files = 1;
                            chunk.error = Some(error);
                            fix.applied = false;
                            results.push(chunk);
                            continue;
                        }
                    }
                }

                fix.applied = true;
                rewrite_callers_after_dedup(fix, root);

                log_status!(
                    "fix",
                    "Applied {} fix(es) to {}",
                    fix.insertions.len(),
                    fix.file
                );
                results.push(chunk);
            }
            Err(e) => {
                results.push(ApplyChunkResult {
                    chunk_id: format!("fix:{}", index + 1),
                    files: vec![fix.file.clone()],
                    status: ChunkStatus::Reverted,
                    applied_files: 0,
                    reverted_files: 0,
                    verification: None,
                    error: Some(format!("Failed to write {}: {}", fix.file, e)),
                });
            }
        }
    }

    results
}
