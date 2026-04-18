use crate::core::refactor::decompose;
use crate::core::refactor::plan::verify::rewrite_callers_after_dedup;

use crate::engine::undo::InMemoryRollback;
use crate::refactor::auto::{ApplyChunkResult, ChunkStatus, DecomposeFixPlan, Fix, NewFile};
use std::path::Path;

// ============================================================================
// EditOp-based apply path (Phase 2 of #1041)
// ============================================================================

/// Apply fixes and new files through the shared `EditOp` engine.
///
/// 1. Converts `Fix` → `Vec<TaggedEditOp>` via `fix_to_edit_ops()`
/// 2. Converts `NewFile` → `TaggedEditOp` via `new_file_to_edit_op()`
/// 3. Calls `apply_edit_ops()` for unified execution (content edits, file moves, file creates)
/// 4. Runs `format_after_write()` on all modified files
/// 5. Runs `rewrite_callers_after_dedup()` for duplicate function fixes
pub fn apply_fixes_via_edit_ops(
    fixes: &mut [Fix],
    new_files: &mut [NewFile],
    root: &Path,
) -> Vec<ApplyChunkResult> {
    use crate::engine::edit_op::{fix_to_edit_ops, new_file_to_edit_op, TaggedEditOp};
    use crate::engine::edit_op_apply::apply_edit_ops;

    // Merge same-file insertions (same as old path)
    merge_same_file_insertions(fixes);

    // Convert all Fix objects to TaggedEditOps
    let mut all_ops: Vec<TaggedEditOp> = Vec::new();
    let mut fix_file_index: Vec<(String, usize)> = Vec::new(); // (file, fix_index)

    for (index, fix) in fixes.iter().enumerate() {
        if fix.insertions.is_empty() {
            continue;
        }
        fix_file_index.push((fix.file.clone(), index));
        all_ops.extend(fix_to_edit_ops(fix));
    }

    // Convert NewFile objects to TaggedEditOps
    let mut new_file_index: Vec<(String, usize)> = Vec::new();
    for (index, nf) in new_files.iter().enumerate() {
        new_file_index.push((nf.file.clone(), index));
        all_ops.push(new_file_to_edit_op(nf));
    }

    if all_ops.is_empty() {
        return Vec::new();
    }

    // Execute all ops through the unified apply path
    let report = match apply_edit_ops(&all_ops, root) {
        Ok(r) => r,
        Err(e) => {
            // Fatal error — return a single reverted chunk
            return vec![ApplyChunkResult {
                chunk_id: "edit_ops:all".to_string(),
                files: fix_file_index
                    .iter()
                    .map(|(f, _)| f.clone())
                    .chain(new_file_index.iter().map(|(f, _)| f.clone()))
                    .collect(),
                status: ChunkStatus::Reverted,
                applied_files: 0,
                reverted_files: 0,
                verification: None,
                error: Some(format!("apply_edit_ops failed: {}", e)),
            }];
        }
    };

    // Collect all modified/created files for formatting
    let mut formatted_files: Vec<std::path::PathBuf> = Vec::new();

    // Build per-fix ApplyChunkResults
    let mut results: Vec<ApplyChunkResult> = Vec::new();

    // Track which files had errors
    let error_files: std::collections::HashSet<&str> =
        report.errors.iter().map(|e| e.file.as_str()).collect();

    for (file, fix_index) in &fix_file_index {
        let fix = &mut fixes[*fix_index];
        if error_files.contains(file.as_str()) {
            let error_msg = report
                .errors
                .iter()
                .find(|e| e.file == *file)
                .map(|e| e.message.clone())
                .unwrap_or_default();
            results.push(ApplyChunkResult {
                chunk_id: format!("fix:{}", fix_index + 1),
                files: vec![file.clone()],
                status: ChunkStatus::Reverted,
                applied_files: 0,
                reverted_files: 0,
                verification: None,
                error: Some(error_msg),
            });
        } else {
            let abs_path = root.join(file);
            formatted_files.push(abs_path);
            fix.applied = true;

            log_status!(
                "fix",
                "Applied {} fix(es) to {}",
                fix.insertions.len(),
                fix.file
            );

            results.push(ApplyChunkResult {
                chunk_id: format!("fix:{}", fix_index + 1),
                files: vec![file.clone()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: Some("write_ok".to_string()),
                error: None,
            });
        }
    }

    // Build per-new-file ApplyChunkResults
    for (file, nf_index) in &new_file_index {
        let nf = &mut new_files[*nf_index];
        if error_files.contains(file.as_str()) {
            let error_msg = report
                .errors
                .iter()
                .find(|e| e.file == *file)
                .map(|e| e.message.clone())
                .unwrap_or_default();
            results.push(ApplyChunkResult {
                chunk_id: format!("new_file:{}", nf_index + 1),
                files: vec![file.clone()],
                status: ChunkStatus::Reverted,
                applied_files: 0,
                reverted_files: 0,
                verification: None,
                error: Some(error_msg),
            });
        } else {
            nf.written = true;
            log_status!("fix", "Created {}", nf.file);

            results.push(ApplyChunkResult {
                chunk_id: format!("new_file:{}", nf_index + 1),
                files: vec![file.clone()],
                status: ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: Some("write_ok".to_string()),
                error: None,
            });
        }
    }

    // Format all modified/created files in one batch
    if !formatted_files.is_empty() {
        let _ = crate::engine::format_write::format_after_write(root, &formatted_files);
    }

    // Run post-apply hooks for duplicate function fixes (caller rewriting)
    for fix in fixes.iter().filter(|f| f.applied) {
        rewrite_callers_after_dedup(fix, root);
    }

    results
}

/// Merge insertions from fixes targeting the same file into the first fix for
/// that file. This ensures all `FunctionRemoval` line ranges are applied against
/// the original file content in a single pass. Without this, the second fix
/// re-reads the already-modified file but uses line numbers from the original,
/// causing brace corruption when lines shift after the first removal.
fn merge_same_file_insertions(fixes: &mut [Fix]) {
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
            left[target_idx].insertions.append(&mut right[0].insertions);
        } else {
            let (left, right) = fixes.split_at_mut(target_idx);
            right[0].insertions.append(&mut left[donor_idx].insertions);
        }
    }
}

pub fn apply_decompose_plans(plans: &mut [DecomposeFixPlan], root: &Path) -> Vec<ApplyChunkResult> {
    let mut results = Vec::new();
    for (index, dfp) in plans.iter_mut().enumerate() {
        let source_abs = root.join(&dfp.file);
        let _source_content = match std::fs::read_to_string(&source_abs) {
            Ok(c) => c,
            Err(e) => {
                results.push(ApplyChunkResult {
                    chunk_id: format!("decompose:{}", index + 1),
                    files: vec![dfp.file.clone()],
                    status: ChunkStatus::Reverted,
                    applied_files: 0,
                    reverted_files: 0,
                    verification: None,
                    error: Some(format!("Failed to read source {}: {}", dfp.file, e)),
                });
                continue;
            }
        };
        let mut rollback = InMemoryRollback::new();
        rollback.capture(&source_abs);
        let mut all_files = vec![dfp.file.clone()];
        for group in &dfp.plan.groups {
            let target_abs = root.join(&group.suggested_target);
            all_files.push(group.suggested_target.clone());
            rollback.capture(&target_abs);
        }

        if let Ok(dry_run_results) = decompose::apply_plan(&dfp.plan, root, false) {
            for mr in &dry_run_results {
                for caller_path in &mr.caller_files_modified {
                    let rel = caller_path
                        .strip_prefix(root)
                        .unwrap_or(caller_path)
                        .to_string_lossy()
                        .to_string();
                    all_files.push(rel);
                    rollback.capture(caller_path);
                }
            }
        }

        match decompose::apply_plan(&dfp.plan, root, true) {
            Ok(move_results) => {
                let files_modified = move_results.iter().filter(|r| r.applied).count();
                let chunk = ApplyChunkResult {
                    chunk_id: format!("decompose:{}", index + 1),
                    files: all_files,
                    status: ChunkStatus::Applied,
                    applied_files: files_modified,
                    reverted_files: 0,
                    verification: Some("decompose_applied".to_string()),
                    error: None,
                };
                dfp.applied = true;
                log_status!(
                    "fix",
                    "Decomposed {} into {} groups",
                    dfp.file,
                    dfp.plan.groups.len()
                );
                results.push(chunk);
            }
            Err(e) => {
                rollback.restore_all();
                results.push(ApplyChunkResult {
                    chunk_id: format!("decompose:{}", index + 1),
                    files: vec![dfp.file.clone()],
                    status: ChunkStatus::Reverted,
                    applied_files: 0,
                    reverted_files: 0,
                    verification: None,
                    error: Some(format!("Decompose failed for {}: {}", dfp.file, e)),
                });
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::AuditFinding;
    use crate::refactor::auto::{Insertion, InsertionKind};

    fn removal_insertion(start_line: usize, end_line: usize, description: &str) -> Insertion {
        Insertion {
            primitive: None,
            kind: InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            },
            finding: AuditFinding::OrphanedTest,
            manual_only: false,
            auto_apply: false,
            blocked_reason: None,
            code: String::new(),
            description: description.to_string(),
        }
    }

    #[test]
    fn merge_same_file_insertions_combines_removals() {
        let mut fixes = vec![
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(108, 111, "Remove orphaned test env_lock")],
                applied: false,
            },
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(
                    151,
                    175,
                    "Remove orphaned test prune_removes",
                )],
                applied: false,
            },
            Fix {
                file: "src/engine/temp.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(
                    177,
                    197,
                    "Remove orphaned test prune_ignores",
                )],
                applied: false,
            },
            Fix {
                file: "src/other.rs".into(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![removal_insertion(10, 20, "Some other file fix")],
                applied: false,
            },
        ];

        merge_same_file_insertions(&mut fixes);

        let temp_fixes_with_insertions: Vec<_> = fixes
            .iter()
            .filter(|f| f.file == "src/engine/temp.rs" && !f.insertions.is_empty())
            .collect();
        assert_eq!(
            temp_fixes_with_insertions.len(),
            1,
            "Only one temp.rs fix should have insertions"
        );
        assert_eq!(
            temp_fixes_with_insertions[0].insertions.len(),
            3,
            "merged fix should have all 3 insertions"
        );

        let empty_temp_fixes = fixes
            .iter()
            .filter(|f| f.file == "src/engine/temp.rs" && f.insertions.is_empty())
            .count();
        assert_eq!(empty_temp_fixes, 2, "donor fixes should be emptied");

        let other_fixes: Vec<_> = fixes.iter().filter(|f| f.file == "src/other.rs").collect();
        assert_eq!(other_fixes.len(), 1);
        assert_eq!(other_fixes[0].insertions.len(), 1);
    }
}
