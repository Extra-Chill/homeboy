use crate::code_audit::conventions::Language;
use crate::core::refactor::decompose;
use crate::core::refactor::plan::generate::primary_type_name_from_declaration;
use crate::core::refactor::plan::verify::rewrite_callers_after_dedup;

use crate::engine::undo::InMemoryRollback;
use crate::refactor::auto::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion,
    InsertionKind, NewFile,
};
use regex::Regex;
use std::path::Path;

/// Apply insertions to file content, returning the modified content.
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
    let mut reexport_removals: Vec<&str> = Vec::new();

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
        result = lines.join("\n");
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

    result
}

/// Remove a function name from `pub use { ... }` blocks.
///
/// Handles both single-line (`pub use module::{a, b, c};`) and multi-line
/// re-export blocks. Removes the name and trailing comma. If the block
/// becomes empty after removal, removes the entire `pub use` statement.
fn remove_from_pub_use_block(lines: &mut Vec<String>, fn_name: &str) {
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
                    // Remove this line entirely if it's just the name
                    let cleaned = word_re
                        .replace(&inner, "")
                        .to_string()
                        .replace(", ,", ",")
                        .trim()
                        .to_string();
                    if cleaned.is_empty() || cleaned == "," {
                        lines.remove(i);
                        continue;
                    }
                    // Remove trailing comma if it's the last item before }
                    let cleaned = cleaned.trim_end_matches(',').trim().to_string();
                    if cleaned.is_empty() {
                        lines.remove(i);
                        continue;
                    }
                    lines[i] = format!(
                        "{}{}",
                        " ".repeat(lines[i].len() - lines[i].trim_start().len()),
                        cleaned
                    );
                }
                if inner.contains('}') {
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

pub(crate) fn insert_into_constructor(
    content: &str,
    stubs: &[&String],
    language: &Language,
) -> String {
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
            let combined: String = stubs
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            insert_before_closing_brace(content, &combined, language)
        }
    }
}

pub(crate) fn insert_namespace_declaration(
    content: &str,
    declaration: &str,
    language: &Language,
) -> String {
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

pub(crate) fn insert_type_conformance(
    content: &str,
    declarations: &[&String],
    language: &Language,
) -> String {
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

pub(crate) fn insert_import(content: &str, import_line: &str, language: &Language) -> String {
    let lines: Vec<&str> = content.lines().collect();

    let import_prefix = match language {
        Language::Rust => "use ",
        Language::Php => "use ",
        Language::JavaScript | Language::TypeScript => "import ",
        Language::Unknown => "use ",
    };

    let rust_definition_starts = [
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "struct ",
        "pub struct ",
        "pub(crate) struct ",
        "enum ",
        "pub enum ",
        "pub(crate) enum ",
        "impl ",
        "impl<",
        "mod ",
        "pub mod ",
        "pub(crate) mod ",
        "trait ",
        "pub trait ",
        "pub(crate) trait ",
        "const ",
        "pub const ",
        "pub(crate) const ",
        "static ",
        "pub static ",
        "pub(crate) static ",
        "type ",
        "pub type ",
        "pub(crate) type ",
        "#[cfg(test)]",
    ];

    let mut last_import_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if *language == Language::Rust
            && rust_definition_starts
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        {
            break;
        }

        if trimmed.starts_with(import_prefix)
            || (trimmed.starts_with("use ") && *language == Language::Rust)
        {
            last_import_idx = Some(i);
        }
    }

    let insert_after = if let Some(idx) = last_import_idx {
        idx
    } else {
        let mut first_code = 0;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with('#')
                || trimmed == "<?php"
            {
                first_code = i + 1;
            } else {
                break;
            }
        }
        if first_code > 0 {
            first_code - 1
        } else {
            0
        }
    };

    let mut result = String::with_capacity(content.len() + import_line.len() + 2);
    for (i, line) in lines.iter().enumerate() {
        result.push_str(line);
        result.push('\n');
        if i == insert_after {
            result.push_str(import_line);
            result.push('\n');
        }
    }

    if !content.ends_with('\n') {
        result.pop();
    }

    result
}

pub(crate) fn insert_before_closing_brace(
    content: &str,
    code: &str,
    _language: &Language,
) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_from_single_line_pub_use() {
        let mut lines: Vec<String> = vec![
            "pub use planner::{analyze_stage_overlaps, build_refactor_plan, normalize_sources};"
                .into(),
        ];
        remove_from_pub_use_block(&mut lines, "analyze_stage_overlaps");
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("analyze_stage_overlaps"));
        assert!(lines[0].contains("build_refactor_plan"));
        assert!(lines[0].contains("normalize_sources"));
    }

    #[test]
    fn remove_last_item_deletes_entire_line() {
        let mut lines: Vec<String> = vec!["pub use planner::{only_function};".into()];
        remove_from_pub_use_block(&mut lines, "only_function");
        assert!(lines.is_empty(), "Empty pub use should be removed entirely");
    }

    #[test]
    fn remove_from_multiline_pub_use() {
        let mut lines: Vec<String> = vec![
            "pub use module::{".into(),
            "    alpha,".into(),
            "    beta,".into(),
            "    gamma,".into(),
            "};".into(),
        ];
        remove_from_pub_use_block(&mut lines, "beta");
        let joined = lines.join("\n");
        assert!(!joined.contains("beta"), "beta should be removed");
        assert!(joined.contains("alpha"), "alpha should remain");
        assert!(joined.contains("gamma"), "gamma should remain");
    }

    #[test]
    fn remove_does_not_touch_unrelated_pub_use() {
        let mut lines: Vec<String> = vec!["pub use other::{foo, bar};".into()];
        remove_from_pub_use_block(&mut lines, "baz");
        assert_eq!(lines[0], "pub use other::{foo, bar};");
    }
}
