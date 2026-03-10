use crate::code_audit::conventions::Language;
use crate::code_audit::fixer::{
    apply_insertions_to_content, detect_language, rewrite_callers_after_dedup, ApplyChunkResult,
    ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion, NewFile,
};
use crate::core::refactor::decompose;
use crate::core::undo::InMemoryRollback;
use std::path::Path;

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

pub fn apply_fixes(fixes: &mut [Fix], root: &Path) -> usize {
    apply_fixes_chunked(fixes, root, ApplyOptions { verifier: None })
        .iter()
        .filter(|chunk| matches!(chunk.status, ChunkStatus::Applied))
        .map(|chunk| chunk.applied_files)
        .sum()
}

pub fn apply_new_files(new_files: &mut [NewFile], root: &Path) -> usize {
    apply_new_files_chunked(new_files, root, ApplyOptions { verifier: None })
        .iter()
        .filter(|chunk| matches!(chunk.status, ChunkStatus::Applied))
        .map(|chunk| chunk.applied_files)
        .sum()
}

pub fn apply_fixes_chunked(
    fixes: &mut [Fix],
    root: &Path,
    options: ApplyOptions<'_>,
) -> Vec<ApplyChunkResult> {
    let mut results = Vec::new();

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

        let language: Language = detect_language(&abs_path);
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

pub fn apply_new_files_chunked(
    new_files: &mut [NewFile],
    root: &Path,
    options: ApplyOptions<'_>,
) -> Vec<ApplyChunkResult> {
    let mut results = Vec::new();

    for (index, nf) in new_files.iter_mut().enumerate() {
        let abs_path = root.join(&nf.file);

        if let Some(parent) = abs_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    results.push(ApplyChunkResult {
                        chunk_id: format!("new_file:{}", index + 1),
                        files: vec![nf.file.clone()],
                        status: ChunkStatus::Reverted,
                        applied_files: 0,
                        reverted_files: 0,
                        verification: None,
                        error: Some(format!("Failed to create directory for {}: {}", nf.file, e)),
                    });
                    continue;
                }
            }
        }

        if abs_path.exists() {
            results.push(ApplyChunkResult {
                chunk_id: format!("new_file:{}", index + 1),
                files: vec![nf.file.clone()],
                status: ChunkStatus::Reverted,
                applied_files: 0,
                reverted_files: 0,
                verification: None,
                error: Some(format!("Skipping {} — file already exists", nf.file)),
            });
            continue;
        }

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&abs_path);

        match std::fs::write(&abs_path, &nf.content) {
            Ok(_) => {
                let mut chunk = ApplyChunkResult {
                    chunk_id: format!("new_file:{}", index + 1),
                    files: vec![nf.file.clone()],
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
                            nf.written = false;
                            results.push(chunk);
                            continue;
                        }
                    }
                }

                nf.written = true;
                log_status!("fix", "Created {}", nf.file);
                results.push(chunk);
            }
            Err(e) => {
                results.push(ApplyChunkResult {
                    chunk_id: format!("new_file:{}", index + 1),
                    files: vec![nf.file.clone()],
                    status: ChunkStatus::Reverted,
                    applied_files: 0,
                    reverted_files: 0,
                    verification: None,
                    error: Some(format!("Failed to create {}: {}", nf.file, e)),
                });
            }
        }
    }

    results
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
