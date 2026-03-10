use crate::code_audit::conventions::Language;
use crate::code_audit::fixer::{
    detect_language, ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix,
    FixResult, Insertion, InsertionKind, NewFile,
};
use crate::core::refactor::decompose;
use crate::core::refactor::plan::audit::rewrite_callers_after_dedup;
use crate::core::undo::InMemoryRollback;
use regex::Regex;
use std::path::Path;

pub(crate) fn apply_insertions_to_content(
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

    if !removals.is_empty() {
        removals.sort_by(|a, b| b.0.cmp(&a.0));
        let mut lines: Vec<&str> = result.lines().collect();
        for (start, end) in &removals {
            let start_idx = start.saturating_sub(1);
            let end_idx = (*end).min(lines.len());
            if start_idx < lines.len() {
                lines.drain(start_idx..end_idx);
            }
        }
        result = lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    if !import_adds.is_empty() {
        result = insert_imports(&result, &import_adds, language);
    }

    if !namespace_declarations.is_empty() {
        result = insert_namespace_declarations(&result, &namespace_declarations, language);
    }

    if !type_conformances.is_empty() {
        result = insert_type_conformances(&result, &type_conformances, language);
    }

    if !constructor_stubs.is_empty() {
        result = insert_constructor_stubs(&result, &constructor_stubs, language);
    }

    if !registration_stubs.is_empty() {
        result = insert_registration_stubs(&result, &registration_stubs, language);
    }

    if !method_stubs.is_empty() {
        result = insert_method_stubs(&result, &method_stubs, language);
    }

    if !trait_uses.is_empty() {
        result = insert_trait_uses(&result, &trait_uses, language);
    }

    result
}

fn insert_imports(content: &str, imports: &[&String], language: &Language) -> String {
    let mut result = content.to_string();
    match language {
        Language::Php => {
            let lines: Vec<&str> = result.lines().collect();
            let insert_at = lines
                .iter()
                .rposition(|line| line.trim_start().starts_with("use "))
                .map(|idx| idx + 1)
                .unwrap_or_else(|| {
                    lines.iter()
                        .position(|line| line.trim_start().starts_with("namespace "))
                        .map(|idx| idx + 1)
                        .unwrap_or(1)
                });
            let mut new_lines = lines.iter().map(|line| (*line).to_string()).collect::<Vec<_>>();
            for import in imports.iter().rev() {
                new_lines.insert(insert_at, (*import).clone());
            }
            result = new_lines.join("\n");
        }
        Language::Rust => {
            let lines: Vec<&str> = result.lines().collect();
            let insert_at = lines
                .iter()
                .rposition(|line| line.trim_start().starts_with("use "))
                .map(|idx| idx + 1)
                .unwrap_or(0);
            let mut new_lines = lines.iter().map(|line| (*line).to_string()).collect::<Vec<_>>();
            for import in imports.iter().rev() {
                new_lines.insert(insert_at, (*import).clone());
            }
            result = new_lines.join("\n");
        }
        Language::JavaScript | Language::TypeScript => {
            let lines: Vec<&str> = result.lines().collect();
            let insert_at = lines
                .iter()
                .rposition(|line| line.trim_start().starts_with("import "))
                .map(|idx| idx + 1)
                .unwrap_or(0);
            let mut new_lines = lines.iter().map(|line| (*line).to_string()).collect::<Vec<_>>();
            for import in imports.iter().rev() {
                new_lines.insert(insert_at, (*import).clone());
            }
            result = new_lines.join("\n");
        }
        Language::Unknown => {}
    }

    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }

    result
}

pub(crate) fn insert_import(content: &str, import_line: &str, language: &Language) -> String {
    let import = import_line.to_string();
    insert_imports(content, &[&import], language)
}

fn insert_namespace_declarations(
    content: &str,
    declarations: &[&String],
    language: &Language,
) -> String {
    declarations.iter().fold(content.to_string(), |acc, declaration| {
        insert_namespace_declaration(&acc, declaration, language)
    })
}

pub(crate) fn insert_namespace_declaration(content: &str, declaration: &str, language: &Language) -> String {
    match language {
        Language::Php => {
            let namespace_re = Regex::new(r"(?m)^\s*namespace\s+[^;]+;").unwrap();
            if namespace_re.is_match(content) {
                return namespace_re.replace(content, declaration).to_string();
            }

            if let Some(open_tag_pos) = content.find("<?php") {
                let insert_pos = open_tag_pos + 5;
                let mut result = String::with_capacity(content.len() + declaration.len() + 2);
                result.push_str(&content[..insert_pos]);
                result.push_str("\n\n");
                result.push_str(declaration);
                result.push_str(&content[insert_pos..]);
                return result;
            }

            format!("{}\n{}", declaration, content)
        }
        _ => content.to_string(),
    }
}

fn insert_type_conformances(
    content: &str,
    declarations: &[&String],
    language: &Language,
) -> String {
    insert_type_conformance(content, declarations, language)
}

pub(crate) fn insert_type_conformance(content: &str, declarations: &[&String], language: &Language) -> String {
    let Some(declaration) = declarations.first() else {
        return content.to_string();
    };

    match language {
        Language::Php | Language::TypeScript => {
            insert_inline_type_conformance(content, declaration, language)
        }
        Language::Rust => {
            if content.contains(declaration.as_str()) {
                content.to_string()
            } else if content.ends_with('\n') {
                format!("{}{}", content, declaration)
            } else {
                format!("{}\n{}", content, declaration)
            }
        }
        Language::JavaScript | Language::Unknown => content.to_string(),
    }
}

fn insert_inline_type_conformance(content: &str, declaration: &str, language: &Language) -> String {
    let conformance = declaration.trim();
    let keyword = match language {
        Language::Php | Language::TypeScript => "implements",
        _ => return content.to_string(),
    };

    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    for line in &mut lines {
        if primary_type_name_from_declaration(line, language).is_none() {
            continue;
        }

        if line.contains(conformance) {
            break;
        }

        if line.contains(keyword) {
            if let Some(pos) = line.find('{') {
                let before = &line[..pos].trim_end();
                let after = &line[pos..];
                *line = format!("{}, {} {}", before, conformance, after);
            } else {
                *line = format!("{}, {}", line.trim_end(), conformance);
            }
        } else if let Some(pos) = line.find('{') {
            let before = line[..pos].trim_end();
            let after = &line[pos..];
            *line = format!("{} {} {} {}", before, keyword, conformance, after);
        } else {
            *line = format!("{} {} {}", line.trim_end(), keyword, conformance);
        }

        break;
    }

    let mut result = lines.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

pub(crate) fn primary_type_name_from_declaration(line: &str, language: &Language) -> Option<String> {
    let trimmed = line.trim();
    match language {
        Language::Php | Language::TypeScript => Regex::new(r"\b(?:class|interface|trait)\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::Rust => Regex::new(r"\b(?:pub\s+)?(?:struct|enum|trait)\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::JavaScript => Regex::new(r"\bclass\s+(\w+)")
            .ok()?
            .captures(trimmed)
            .map(|cap| cap[1].to_string()),
        Language::Unknown => None,
    }
}

fn insert_constructor_stubs(content: &str, stubs: &[&String], language: &Language) -> String {
    let combined: String = stubs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("");
    insert_before_closing_brace(content, &combined, language)
}

fn insert_registration_stubs(content: &str, stubs: &[&String], language: &Language) -> String {
    insert_into_constructor(content, stubs, language)
}

fn insert_method_stubs(content: &str, stubs: &[&String], language: &Language) -> String {
    let combined: String = stubs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("");
    insert_before_closing_brace(content, &combined, language)
}

pub(crate) fn insert_into_constructor(content: &str, stubs: &[&String], language: &Language) -> String {
    let constructor_pattern = match language {
        Language::Php => r"function\s+__construct\s*\([^)]*\)\s*\{",
        Language::Rust => r"fn\s+new\s*\([^)]*\)\s*(?:->[^{]*)?\{",
        Language::JavaScript | Language::TypeScript => r"constructor\s*\([^)]*\)\s*\{",
        Language::Unknown => return content.to_string(),
    };

    let re = match Regex::new(constructor_pattern) {
        Ok(re) => re,
        Err(_) => return content.to_string(),
    };

    if let Some(m) = re.find(content) {
        let insert_pos = m.end();
        let insert_text: String = stubs.iter().map(|s| format!("\n{}", s)).collect();

        let mut result = String::with_capacity(content.len() + insert_text.len());
        result.push_str(&content[..insert_pos]);
        result.push_str(&insert_text);
        result.push_str(&content[insert_pos..]);
        result
    } else {
        content.to_string()
    }
}

pub(crate) fn insert_trait_uses(content: &str, stubs: &[&String], language: &Language) -> String {
    match language {
        Language::Php => {
            let class_re = Regex::new(r"(?:class|trait|interface)\s+\w+[^\{]*\{").unwrap();
            if let Some(m) = class_re.find(content) {
                let insert_pos = m.end();
                let mut result = String::with_capacity(content.len() + stubs.len() * 40);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                for stub in stubs {
                    result.push_str(stub.trim_end());
                    result.push('\n');
                }
                result.push_str(&content[insert_pos..]);
                result
            } else {
                content.to_string()
            }
        }
        _ => {
            let combined: String = stubs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
            insert_before_closing_brace(content, &combined, language)
        }
    }
}

pub(crate) fn insert_before_closing_brace(content: &str, code: &str, _language: &Language) -> String {
    if let Some(last_brace) = content.rfind('}') {
        let mut result = String::with_capacity(content.len() + code.len());
        result.push_str(&content[..last_brace]);
        result.push_str(code);
        result.push_str(&content[last_brace..]);
        result
    } else {
        format!("{}{}", content, code)
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
