use crate::code_audit::conventions::Language;
use crate::code_audit::{AuditFinding, CodeAuditResult, DuplicateGroup};
use crate::core::refactor::auto::{Fix, FixSafetyTier, InsertionKind, NewFile, SkippedFile};
use crate::core::refactor::shared::detect_language;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;

use super::{find_parsed_item_by_name, insertion, new_file, parse_items_for_dedup};

pub(crate) fn extract_function_name_from_unreferenced(description: &str) -> Option<String> {
    let needle = "Public function '";
    let start = description.find(needle)? + needle.len();
    let rest = &description[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

pub(crate) fn module_path_from_file(file_path: &str) -> String {
    crate::core::engine::symbol_graph::module_path_from_file(file_path)
}

pub(crate) fn generate_unreferenced_export_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    for finding in &result.findings {
        if finding.kind != AuditFinding::UnreferencedExport {
            continue;
        }

        let Some(fn_name) = extract_function_name_from_unreferenced(&finding.description) else {
            continue;
        };

        let abs_path = root.join(&finding.file);
        let language = detect_language(&abs_path);
        if !matches!(language, Language::Rust) {
            continue;
        }

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(content) => content,
            Err(_) => continue,
        };

        if content.contains(&format!("pub(crate) fn {}", fn_name))
            || content.contains(&format!("pub(super) fn {}", fn_name))
        {
            continue;
        }

        if is_reexported(&finding.file, &fn_name, root) {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Function '{}' is re-exported or used by binary crate — cannot narrow visibility",
                    fn_name
                ),
            });
            continue;
        }

        let target_patterns = [
            format!("pub fn {}(", fn_name),
            format!("pub fn {}<", fn_name),
            format!("pub async fn {}(", fn_name),
            format!("pub async fn {}<", fn_name),
        ];

        let found_line = content.lines().enumerate().find_map(|(index, line)| {
            let trimmed = line.trim();
            target_patterns
                .iter()
                .any(|pat| trimmed.contains(pat.as_str()))
                .then_some(index + 1)
        });

        let Some(line_num) = found_line else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!("Could not locate `pub fn {}` declaration in file", fn_name),
            });
            continue;
        };

        let line_content = content.lines().nth(line_num - 1).unwrap_or("");
        let (from, to) = if line_content.contains(&format!("pub async fn {}", fn_name)) {
            (
                "pub async fn".to_string(),
                "pub(crate) async fn".to_string(),
            )
        } else {
            ("pub fn".to_string(), "pub(crate) fn".to_string())
        };

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![insertion(
                InsertionKind::VisibilityChange {
                    line: line_num,
                    from: from.clone(),
                    to: to.clone(),
                },
                AuditFinding::UnreferencedExport,
                format!("{} → {}", from, to),
                format!(
                    "Narrow visibility of '{}': {} → {} (unreferenced export)",
                    fn_name, from, to
                ),
            )],
            applied: false,
        });
    }
}

pub(crate) fn generate_duplicate_function_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    new_files: &mut Vec<NewFile>,
    skipped: &mut Vec<SkippedFile>,
) {
    const MIN_EXTRACT_GROUP_SIZE: usize = 4;
    const SKIP_EXTRACT_NAMES: &[&str] = &[
        "__construct",
        "constructor",
        "new",
        "set_up",
        "setUp",
        "tear_down",
        "tearDown",
    ];

    for group in &result.duplicate_groups {
        let group_size = 1 + group.remove_from.len();

        if SKIP_EXTRACT_NAMES.contains(&group.function_name.as_str()) {
            continue;
        }

        if group_size < MIN_EXTRACT_GROUP_SIZE {
            generate_simple_duplicate_fixes(group, root, fixes, skipped);
            continue;
        }

        let canonical_abs = root.join(&group.canonical_file);
        let ext = canonical_abs
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let language = detect_language(&canonical_abs);
        let use_extract_shared = matches!(language, Language::Php)
            && !crate::code_audit::is_test_path(&group.canonical_file);

        let canonical_content = match std::fs::read_to_string(&canonical_abs) {
            Ok(content) => content,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: group.canonical_file.clone(),
                    reason: format!(
                        "Cannot read canonical file for duplicate `{}`",
                        group.function_name
                    ),
                });
                continue;
            }
        };

        let manifest = if use_extract_shared {
            crate::extension::find_extension_for_file_ext(ext, "refactor")
        } else {
            None
        };

        let Some(manifest) = manifest else {
            generate_simple_duplicate_fixes(group, root, fixes, skipped);
            continue;
        };

        let mut file_entries = Vec::new();
        let mut any_read_failure = false;
        for remove_file in &group.remove_from {
            let abs_path = root.join(remove_file);
            match std::fs::read_to_string(&abs_path) {
                Ok(content) => {
                    file_entries.push(serde_json::json!({
                        "path": remove_file,
                        "content": content,
                    }));
                }
                Err(_) => {
                    skipped.push(SkippedFile {
                        file: remove_file.clone(),
                        reason: format!(
                            "Cannot read file to remove duplicate `{}`",
                            group.function_name
                        ),
                    });
                    any_read_failure = true;
                }
            }
        }
        if any_read_failure && file_entries.is_empty() {
            continue;
        }

        let mut all_paths: Vec<&str> = vec![group.canonical_file.as_str()];
        all_paths.extend(group.remove_from.iter().map(|path| path.as_str()));

        let extract_cmd = serde_json::json!({
            "command": "extract_shared",
            "function_name": group.function_name,
            "canonical_file": group.canonical_file,
            "canonical_content": canonical_content,
            "files": file_entries,
            "all_file_paths": all_paths,
        });

        let Some(result_val) = crate::extension::run_refactor_script(&manifest, &extract_cmd)
        else {
            generate_simple_duplicate_fixes(group, root, fixes, skipped);
            continue;
        };

        if result_val.get("error").is_some() {
            let err = result_val["error"].as_str().unwrap_or("unknown error");
            skipped.push(SkippedFile {
                file: group.canonical_file.clone(),
                reason: format!(
                    "extract_shared failed for `{}`: {}",
                    group.function_name, err
                ),
            });
            continue;
        }

        if result_val
            .get("skip")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let reason = result_val
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("extension decided to skip");
            skipped.push(SkippedFile {
                file: group.canonical_file.clone(),
                reason: format!("Skipped `{}`: {}", group.function_name, reason),
            });
            continue;
        }

        if let (Some(trait_file), Some(trait_content)) = (
            result_val
                .get("trait_file")
                .and_then(|value| value.as_str()),
            result_val
                .get("trait_content")
                .and_then(|value| value.as_str()),
        ) {
            if !new_files.iter().any(|new_file| new_file.file == trait_file) {
                let trait_name = result_val
                    .get("trait_name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("SharedTrait");
                new_files.push(new_file(
                    AuditFinding::DuplicateFunction,
                    FixSafetyTier::PlanOnly,
                    trait_file.to_string(),
                    trait_content.to_string(),
                    format!(
                        "Create trait `{}` for shared `{}` method",
                        trait_name, group.function_name
                    ),
                ));
            }
        }

        if let Some(file_edits) = result_val
            .get("file_edits")
            .and_then(|value| value.as_array())
        {
            for edit in file_edits {
                let Some(file) = edit
                    .get("file")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                else {
                    continue;
                };

                let mut insertions = Vec::new();

                if let Some(remove_lines) = edit.get("remove_lines") {
                    if let (Some(start), Some(end)) = (
                        remove_lines
                            .get("start_line")
                            .and_then(|value| value.as_u64()),
                        remove_lines
                            .get("end_line")
                            .and_then(|value| value.as_u64()),
                    ) {
                        insertions.push(insertion(
                            InsertionKind::FunctionRemoval {
                                start_line: start as usize,
                                end_line: end as usize,
                            },
                            AuditFinding::DuplicateFunction,
                            String::new(),
                            format!(
                                "Remove duplicate `{}` (extracted to shared trait)",
                                group.function_name
                            ),
                        ));
                    }
                }

                if let Some(import) = edit.get("add_import").and_then(|value| value.as_str()) {
                    insertions.push(insertion(
                        InsertionKind::ImportAdd,
                        AuditFinding::DuplicateFunction,
                        import.to_string(),
                        format!("Import shared trait for `{}`", group.function_name),
                    ));
                }

                if let Some(use_trait) = edit.get("add_use_trait").and_then(|value| value.as_str())
                {
                    insertions.push(insertion(
                        InsertionKind::TraitUse,
                        AuditFinding::DuplicateFunction,
                        use_trait.to_string(),
                        format!("Use shared trait for `{}`", group.function_name),
                    ));
                }

                if !insertions.is_empty() {
                    fixes.push(Fix {
                        file,
                        required_methods: vec![],
                        required_registrations: vec![],
                        insertions,
                        applied: false,
                    });
                }
            }
        }
    }
}

fn generate_simple_duplicate_fixes(
    group: &DuplicateGroup,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    for remove_file in &group.remove_from {
        let abs_path = root.join(remove_file.as_str());
        let ext = abs_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(content) => content,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: remove_file.clone(),
                    reason: format!(
                        "Cannot read file to remove duplicate `{}`",
                        group.function_name
                    ),
                });
                continue;
            }
        };

        let items = parse_items_for_dedup(ext, &content, remove_file);
        let Some(items) = items else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Cannot locate `{}` boundaries in {} — no grammar or extension available",
                    group.function_name, remove_file
                ),
            });
            continue;
        };

        let Some(item) = find_parsed_item_by_name(&items, &group.function_name) else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Function `{}` not found by parser in {}",
                    group.function_name, remove_file
                ),
            });
            continue;
        };

        let import_path = module_path_from_file(&group.canonical_file);
        let import_stmt = match ext {
            "rs" => format!("use crate::{}::{};", import_path, group.function_name),
            _ => format!(
                "import {{ {} }} from '{}';",
                group.function_name, import_path
            ),
        };

        let mut insertions = vec![insertion(
            InsertionKind::FunctionRemoval {
                start_line: item.start_line,
                end_line: item.end_line,
            },
            AuditFinding::DuplicateFunction,
            String::new(),
            format!(
                "Remove duplicate `{}` (canonical copy in {})",
                group.function_name, group.canonical_file
            ),
        )];

        if !content.contains(&import_stmt) {
            insertions.push(insertion(
                InsertionKind::ImportAdd,
                AuditFinding::DuplicateFunction,
                import_stmt,
                format!("Import `{}` from canonical location", group.function_name),
            ));
        }

        fixes.push(Fix {
            file: remove_file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions,
            applied: false,
        });
    }
}

pub(crate) fn is_reexported(file_path: &str, fn_name: &str, root: &Path) -> bool {
    let source_path = Path::new(file_path);

    let mut current = source_path.parent();
    while let Some(dir) = current {
        for filename in &["mod.rs", "lib.rs"] {
            let check_path = root.join(dir).join(filename);
            if check_path.exists()
                && std::fs::read_to_string(&check_path)
                    .ok()
                    .is_some_and(|content| has_pub_use_of(&content, fn_name))
            {
                return true;
            }
        }
        current = dir.parent();
    }

    is_used_by_binary_crate(fn_name, root)
}

pub(crate) fn has_pub_use_of(content: &str, fn_name: &str) -> bool {
    let word_re = match Regex::new(&format!(r"\b{}\b", regex::escape(fn_name))) {
        Ok(re) => re,
        Err(_) => return false,
    };

    let mut in_pub_use_block = false;
    for line in content.lines() {
        let trimmed = line.trim();

        if in_pub_use_block {
            if word_re.is_match(trimmed) {
                return true;
            }
            if trimmed.contains("};") || trimmed == "}" {
                in_pub_use_block = false;
            }
        } else if trimmed.starts_with("pub use") {
            // Skip glob re-exports like `pub use core::*;` — they make
            // the name accessible but the audit already checked whether
            // anyone actually references it.
            if trimmed.contains("::*") {
                continue;
            }
            if word_re.is_match(trimmed) {
                return true;
            }
            if trimmed.contains('{') && !trimmed.contains('}') {
                in_pub_use_block = true;
            }
        }
    }
    false
}

fn is_used_by_binary_crate(fn_name: &str, root: &Path) -> bool {
    let word_re = match Regex::new(&format!(r"\b{}\b", regex::escape(fn_name))) {
        Ok(re) => re,
        Err(_) => return false,
    };

    let src = root.join("src");
    let main_rs = src.join("main.rs");
    if main_rs.exists()
        && std::fs::read_to_string(&main_rs)
            .ok()
            .is_some_and(|content| word_re.is_match(&content))
    {
        return true;
    }

    let lib_rs = src.join("lib.rs");
    let lib_mods = if lib_rs.exists() {
        std::fs::read_to_string(&lib_rs)
            .ok()
            .map(|content| extract_mod_names(&content))
            .unwrap_or_default()
    } else {
        HashSet::new()
    };

    let main_mods = if main_rs.exists() {
        std::fs::read_to_string(&main_rs)
            .ok()
            .map(|content| extract_mod_names(&content))
            .unwrap_or_default()
    } else {
        HashSet::new()
    };

    for mod_name in main_mods.difference(&lib_mods) {
        let mod_dir = src.join(mod_name);
        if mod_dir.is_dir() && scan_dir_for_reference(&mod_dir, &word_re) {
            return true;
        }

        let mod_file = src.join(format!("{}.rs", mod_name));
        if mod_file.exists()
            && std::fs::read_to_string(&mod_file)
                .ok()
                .is_some_and(|content| word_re.is_match(&content))
        {
            return true;
        }
    }

    false
}

fn extract_mod_names(content: &str) -> HashSet<String> {
    let mut mods = HashSet::new();
    let re = Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)\s*;").unwrap();
    for cap in re.captures_iter(content) {
        mods.insert(cap[1].to_string());
    }
    mods
}

fn scan_dir_for_reference(dir: &Path, word_re: &Regex) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if scan_dir_for_reference(&path, word_re) {
                return true;
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|content| word_re.is_match(&content))
        {
            return true;
        }
    }

    false
}
