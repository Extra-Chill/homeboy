//! apply — extracted from apply.rs.

use crate::code_audit::conventions::Language;
use crate::core::refactor::decompose;
use crate::engine::undo::InMemoryRollback;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use std::path::Path;
use regex::Regex;
use super::remove_from_pub_use_block;
use super::insert_into_constructor;
use super::insert_before_closing_brace;
use super::insert_type_conformance;
use super::insert_namespace_declaration;
use super::insert_trait_uses;
use super::insert_import;


/// Apply insertions to file content, returning the modified content.
pub fn apply_insertions_to_content(
    content: &str,
    insertions: &[Insertion],
    language: &Language,
) -> String {
    let mut result = content.to_string();

    let mut method_stubs = Vec::new();
    let mut registration_stubs = Vec::new();
    let mut constructor_stubs = Vec::new();
    let mut import_adds = Vec::new();
    let mut type_conformances = Vec::new();
    let mut namespace_declarations = Vec::new();
    let mut trait_uses = Vec::new();
    let mut removals: Vec<(usize, usize)> = Vec::new();
    let mut visibility_changes: Vec<(usize, &str, &str)> = Vec::new();
    let mut doc_ref_updates: Vec<(usize, &str, &str)> = Vec::new();
    let mut doc_line_removals: Vec<usize> = Vec::new();
    let mut reexport_removals: Vec<&str> = Vec::new();
    let mut line_replacements: Vec<(usize, &str, &str)> = Vec::new();
    let mut test_modules: Vec<&str> = Vec::new();

    for insertion in insertions {
        match &insertion.kind {
            InsertionKind::MethodStub => method_stubs.push(&insertion.code),
            InsertionKind::RegistrationStub => registration_stubs.push(&insertion.code),
            InsertionKind::ConstructorWithRegistration => constructor_stubs.push(&insertion.code),
            InsertionKind::ImportAdd => import_adds.push(&insertion.code),
            InsertionKind::TypeConformance => type_conformances.push(&insertion.code),
            InsertionKind::NamespaceDeclaration => namespace_declarations.push(&insertion.code),
            InsertionKind::TraitUse => trait_uses.push(&insertion.code),
            InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            } => removals.push((*start_line, *end_line)),
            InsertionKind::VisibilityChange { line, from, to } => {
                visibility_changes.push((*line, from.as_str(), to.as_str()));
            }
            InsertionKind::DocReferenceUpdate {
                line,
                old_ref,
                new_ref,
            } => doc_ref_updates.push((*line, old_ref.as_str(), new_ref.as_str())),
            InsertionKind::DocLineRemoval { line } => doc_line_removals.push(*line),
            InsertionKind::ReexportRemoval { fn_name } => {
                reexport_removals.push(fn_name.as_str());
            }
            InsertionKind::LineReplacement {
                line,
                old_text,
                new_text,
            } => line_replacements.push((*line, old_text.as_str(), new_text.as_str())),
            InsertionKind::FileMove { .. } => {
                // File moves are handled by apply_file_moves(), not content transformation.
            }
            InsertionKind::TestModule => test_modules.push(&insertion.code),
        }
    }

    if !visibility_changes.is_empty() {
        let mut lines: Vec<String> = result.lines().map(String::from).collect();
        for (line_num, from, to) in &visibility_changes {
            let idx = line_num.saturating_sub(1);
            if idx < lines.len() {
                lines[idx] = lines[idx].replacen(from, to, 1);
            }
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    if !line_replacements.is_empty() {
        let mut lines: Vec<String> = result.lines().map(String::from).collect();
        for (line_num, old_text, new_text) in &line_replacements {
            let idx = line_num.saturating_sub(1);
            if idx < lines.len() {
                lines[idx] = lines[idx].replacen(old_text, new_text, 1);
            }
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    if !doc_ref_updates.is_empty() {
        let mut lines: Vec<String> = result.lines().map(String::from).collect();
        for (line_num, old_ref, new_ref) in &doc_ref_updates {
            let idx = line_num.saturating_sub(1);
            if idx < lines.len() {
                lines[idx] = lines[idx].replacen(old_ref, new_ref, 1);
            }
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    if !doc_line_removals.is_empty() {
        let mut lines: Vec<String> = result.lines().map(String::from).collect();
        doc_line_removals.sort_unstable_by(|a, b| b.cmp(a));
        for line_num in doc_line_removals {
            let idx = line_num.saturating_sub(1);
            if idx < lines.len() {
                lines.remove(idx);
            }
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    // Remove function names from `pub use { ... }` re-export blocks.
    if !reexport_removals.is_empty() {
        let mut lines: Vec<String> = result.lines().map(String::from).collect();
        for fn_name in &reexport_removals {
            remove_from_pub_use_block(&mut lines, fn_name);
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    if !removals.is_empty() {
        removals.sort_by(|a, b| b.0.cmp(&a.0));
        let mut lines: Vec<&str> = result.lines().collect();
        for (start, end) in &removals {
            let start_idx = start.saturating_sub(1);
            let end_idx = (*end).min(lines.len());
            if start_idx < lines.len() {
                let remove_end = if end_idx < lines.len() && lines[end_idx].trim().is_empty() {
                    end_idx + 1
                } else {
                    end_idx
                };
                lines.drain(start_idx..remove_end);
            }
        }
        // Collapse consecutive blank lines left behind by multiple removals.
        // Without this, removing several adjacent imports leaves gaps that
        // cause `cargo fmt --check` to fail in the validation stage.
        let mut collapsed = Vec::with_capacity(lines.len());
        let mut prev_blank = false;
        for line in &lines {
            let is_blank = line.trim().is_empty();
            if is_blank && prev_blank {
                continue;
            }
            collapsed.push(*line);
            prev_blank = is_blank;
        }
        result = collapsed.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    for declaration in &namespace_declarations {
        result = insert_namespace_declaration(&result, declaration, language);
    }

    for import_line in &import_adds {
        result = insert_import(&result, import_line, language);
    }

    if !type_conformances.is_empty() {
        result = insert_type_conformance(&result, &type_conformances, language);
    }

    if !trait_uses.is_empty() {
        result = insert_trait_uses(&result, &trait_uses, language);
    }

    if !registration_stubs.is_empty() {
        result = insert_into_constructor(&result, &registration_stubs, language);
    }

    if !constructor_stubs.is_empty() {
        let combined: String = constructor_stubs
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("");
        result = insert_before_closing_brace(&result, &combined, language);
    }

    if !method_stubs.is_empty() {
        let combined: String = method_stubs
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("");
        result = insert_before_closing_brace(&result, &combined, language);
    }

    // Append test modules at the end of the file
    for test_module in &test_modules {
        result.push_str(test_module);
    }

    result
}

pub fn apply_decompose_plans(
    plans: &mut [DecomposeFixPlan],
    root: &Path,
    options: ApplyOptions<'_>,
) -> Vec<ApplyChunkResult> {
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
                let mut chunk = ApplyChunkResult {
                    chunk_id: format!("decompose:{}", index + 1),
                    files: all_files,
                    status: ChunkStatus::Applied,
                    applied_files: files_modified,
                    reverted_files: 0,
                    verification: Some("decompose_applied".to_string()),
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
                            chunk.reverted_files = files_modified;
                            chunk.error = Some(error);
                            dfp.applied = false;
                            results.push(chunk);
                            continue;
                        }
                    }
                }
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

/// Apply file move operations from fixes.
///
/// Extracts all `InsertionKind::FileMove` from fixes, executes them via
/// `refactor move --file`, and returns the number of files moved.
///
/// Runs after content fixes so moved files contain their updated content.
pub fn apply_file_moves(fixes: &[Fix], root: &Path) -> Vec<ApplyChunkResult> {
    let mut results = Vec::new();

    for fix in fixes {
        for insertion in &fix.insertions {
            if let InsertionKind::FileMove { from, to } = &insertion.kind {
                let from_abs = root.join(from);
                let to_abs = root.join(to);

                // Validate source exists
                if !from_abs.exists() {
                    results.push(ApplyChunkResult {
                        chunk_id: format!("file_move:{}", from),
                        files: vec![from.clone(), to.clone()],
                        status: ChunkStatus::Reverted,
                        applied_files: 0,
                        reverted_files: 0,
                        verification: None,
                        error: Some(format!("Source file does not exist: {}", from)),
                    });
                    continue;
                }

                // Skip if destination already exists
                if to_abs.exists() {
                    results.push(ApplyChunkResult {
                        chunk_id: format!("file_move:{}", from),
                        files: vec![from.clone(), to.clone()],
                        status: ChunkStatus::Reverted,
                        applied_files: 0,
                        reverted_files: 0,
                        verification: None,
                        error: Some(format!("Destination already exists: {}", to)),
                    });
                    continue;
                }

                // Create parent directories
                if let Some(parent) = to_abs.parent() {
                    if !parent.exists() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            results.push(ApplyChunkResult {
                                chunk_id: format!("file_move:{}", from),
                                files: vec![from.clone(), to.clone()],
                                status: ChunkStatus::Reverted,
                                applied_files: 0,
                                reverted_files: 0,
                                verification: None,
                                error: Some(format!("Failed to create directory: {}", e)),
                            });
                            continue;
                        }
                    }
                }

                // Execute the move
                match std::fs::rename(&from_abs, &to_abs) {
                    Ok(_) => {
                        crate::log_status!("move", "Moved {} → {}", from, to);
                        results.push(ApplyChunkResult {
                            chunk_id: format!("file_move:{}", from),
                            files: vec![from.clone(), to.clone()],
                            status: ChunkStatus::Applied,
                            applied_files: 1,
                            reverted_files: 0,
                            verification: None,
                            error: None,
                        });
                    }
                    Err(e) => {
                        results.push(ApplyChunkResult {
                            chunk_id: format!("file_move:{}", from),
                            files: vec![from.clone(), to.clone()],
                            status: ChunkStatus::Reverted,
                            applied_files: 0,
                            reverted_files: 0,
                            verification: None,
                            error: Some(format!("Move failed: {}", e)),
                        });
                    }
                }
            }
        }
    }

    results
}
