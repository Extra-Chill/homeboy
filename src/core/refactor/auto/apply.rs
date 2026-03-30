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
            static CLASS_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                Regex::new(r"(?:class|trait|interface)\s+\w+[^\{]*\{").unwrap()
            });
            if let Some(m) = CLASS_RE.find(content) {
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
    if import_already_present(content, import_line, language) {
        return content.to_string();
    }

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

fn import_already_present(content: &str, import_line: &str, language: &Language) -> bool {
    let normalized_candidate = normalize_import_line(import_line);
    if normalized_candidate.is_empty() {
        return true;
    }

    content.lines().any(|line| {
        let trimmed = line.trim();
        if !is_import_line(trimmed, language) {
            return false;
        }
        normalize_import_line(trimmed) == normalized_candidate
    })
}

fn is_import_line(line: &str, language: &Language) -> bool {
    match language {
        Language::Rust | Language::Php | Language::Unknown => line.starts_with("use "),
        Language::JavaScript | Language::TypeScript => line.starts_with("import "),
    }
}

fn normalize_import_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_import_skips_identical_existing_rust_import() {
        let content = "use std::collections::HashMap;\n\npub fn run() {}\n";
        let result = insert_import(content, "use std::collections::HashMap;", &Language::Rust);
        assert_eq!(result, content);
    }

    #[test]
    fn insert_import_skips_equivalent_rust_import_with_spacing_differences() {
        let content = "use std::path::{Path, PathBuf};\n\npub fn run() {}\n";
        let result = insert_import(
            content,
            "use  std::path::{Path,   PathBuf};",
            &Language::Rust,
        );
        assert_eq!(result, content);
    }

    #[test]
    fn merge_same_file_insertions_combines_removals() {
        // Simulate the temp.rs scenario: 3 orphaned tests in the same file,
        // each generating a separate Fix with one FunctionRemoval.
        use crate::code_audit::AuditFinding;
        fn removal_insertion(start: usize, end: usize, desc: &str) -> Insertion {
            Insertion {
                primitive: None,
                kind: InsertionKind::FunctionRemoval {
                    start_line: start,
                    end_line: end,
                },
                finding: AuditFinding::OrphanedTest,
                manual_only: false,
                code: String::new(),
                description: desc.into(),
                auto_apply: true,
                blocked_reason: None,
            }
        }

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

        // The first temp.rs fix should have all 3 insertions merged into it
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

        // Donor fixes should be emptied (insertions drained)
        let empty_temp_fixes = fixes
            .iter()
            .filter(|f| f.file == "src/engine/temp.rs" && f.insertions.is_empty())
            .count();
        assert_eq!(empty_temp_fixes, 2, "donor fixes should be emptied");

        // The other.rs fix should be untouched
        let other_fixes: Vec<_> = fixes.iter().filter(|f| f.file == "src/other.rs").collect();
        assert_eq!(other_fixes.len(), 1);
        assert_eq!(other_fixes[0].insertions.len(), 1);
    }

    #[test]
    fn multiple_removals_same_file_preserve_braces() {
        // Reproduce the temp.rs brace corruption: a test module with multiple
        // test functions removed. The mod tests closing brace must survive.
        let content = "\
fn source_fn() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> i32 {
        42
    }

}
";
        use crate::code_audit::AuditFinding;
        fn removal(start: usize, end: usize, desc: &str) -> Insertion {
            Insertion {
                primitive: None,
                kind: InsertionKind::FunctionRemoval {
                    start_line: start,
                    end_line: end,
                },
                finding: AuditFinding::OrphanedTest,
                manual_only: false,
                code: String::new(),
                description: desc.into(),
                auto_apply: true,
                blocked_reason: None,
            }
        }

        // Remove all three test functions (lines are 1-indexed):
        // test_alpha: #[test] at 11, fn at 12, body 13, } at 14
        // test_beta:  #[test] at 16, fn at 17, body 18-19, } at 20
        // test_gamma: #[test] at 22, fn at 23, body 24-25, } at 26
        let insertions = vec![
            removal(11, 14, "Remove test_alpha"),
            removal(16, 20, "Remove test_beta"),
            removal(22, 26, "Remove test_gamma"),
        ];

        let result = apply_insertions_to_content(content, &insertions, &Language::Rust);

        // The mod tests block must still be properly closed
        let open_braces = result.matches('{').count();
        let close_braces = result.matches('}').count();
        assert_eq!(
            open_braces, close_braces,
            "Braces must be balanced after removal.\nResult:\n{result}"
        );
        assert!(
            result.contains("mod tests {"),
            "mod tests should still exist"
        );
        // helper() should survive — it wasn't removed
        assert!(
            result.contains("fn helper()"),
            "helper function should survive"
        );
    }
}
