use crate::core::code_audit::conventions::Language;
use crate::core::code_audit::{AuditFinding, CodeAuditResult, DuplicateGroup};
use crate::core::refactor::auto::{Fix, InsertionKind, NewFile, SkippedFile};

use regex::Regex;
use std::collections::HashSet;
use std::path::Path;

use super::{
    find_parsed_item_by_name, function_removal, insertion, new_file, parse_items_for_dedup,
    FileRole, ModuleSurfaceIndex,
};

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

/// Generate a language-appropriate import statement for a duplicate function fix.
///
/// For Rust: `use crate::module::path::function_name;`
/// For PHP: `use Namespace\ClassName;` (reads namespace from canonical file)
/// For JS/TS: `import { function_name } from 'module/path';`
fn generate_duplicate_import(
    canonical_file: &str,
    function_name: &str,
    language: &Language,
    root: &Path,
) -> String {
    match language {
        Language::Rust => {
            let import_path = module_path_from_file(canonical_file);
            format!("use crate::{}::{};", import_path, function_name)
        }
        Language::Php => {
            // Read the canonical file to extract its namespace and class name
            let canonical_abs = root.join(canonical_file);
            let content = std::fs::read_to_string(&canonical_abs).unwrap_or_default();
            if let Some(fqcn) = extract_php_fqcn(&content) {
                format!("use {};", fqcn)
            } else {
                // Fallback: derive from file path (inc/Abilities/Foo.php → Abilities\Foo)
                let stem = Path::new(canonical_file)
                    .with_extension("")
                    .to_string_lossy()
                    .replace('/', "\\");
                format!("use {};", stem)
            }
        }
        Language::JavaScript | Language::TypeScript => {
            let import_path = module_path_from_file(canonical_file);
            let name = import_path
                .rsplit("::")
                .next()
                .or_else(|| import_path.rsplit('/').next())
                .unwrap_or(&import_path);
            format!("import {{ {} }} from '{}';", name, import_path)
        }
        Language::Unknown => {
            let import_path = module_path_from_file(canonical_file);
            format!("use {};", import_path)
        }
    }
}

/// Extract the fully qualified class name (namespace + class) from PHP file content.
///
/// Reads `namespace Foo\Bar;` and `class Baz` declarations to produce `Foo\Bar\Baz`.
fn extract_php_fqcn(content: &str) -> Option<String> {
    let mut namespace = None;
    let mut class_name = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if namespace.is_none() {
            if let Some(ns) = trimmed
                .strip_prefix("namespace ")
                .and_then(|rest| rest.strip_suffix(';'))
            {
                namespace = Some(ns.trim().to_string());
            }
        }

        if class_name.is_none() {
            // Match: class Foo, abstract class Foo, final class Foo
            if let Some(rest) = trimmed.strip_prefix("class ").or_else(|| {
                trimmed
                    .strip_prefix("abstract class ")
                    .or_else(|| trimmed.strip_prefix("final class "))
            }) {
                // Class name is the first word
                let name = rest.split_whitespace().next()?;
                class_name = Some(name.to_string());
            }
        }

        if namespace.is_some() && class_name.is_some() {
            break;
        }
    }

    match (namespace, class_name) {
        (Some(ns), Some(cls)) => Some(format!("{}\\{}", ns, cls)),
        (None, Some(cls)) => Some(cls),
        _ => None,
    }
}

pub(crate) fn generate_unreferenced_export_fixes(
    result: &CodeAuditResult,
    root: &Path,
    module_surfaces: &ModuleSurfaceIndex,
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
        let language = Language::from_path(&abs_path);
        if !matches!(language, Language::Rust) {
            continue;
        }

        let Some(surface) = module_surfaces.get(&finding.file) else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: "Missing module surface for file".to_string(),
            });
            continue;
        };

        if matches!(surface.role, FileRole::Index | FileRole::PublicApi) {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Module surface marks '{}' as API/barrel file — keep export in place",
                    finding.file
                ),
            });
            continue;
        }

        let Some(symbol_surface) = surface.symbol_surface(&fn_name) else {
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Module surface has no export record for '{}' in {}",
                    fn_name, finding.file
                ),
            });
            continue;
        };

        if symbol_surface.has_external_usage(&finding.file) {
            skip_file(
                skipped,
                finding.file.clone(),
                external_usage_reason(&fn_name),
            );
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

        // Hard block: binary crate directly references this function.
        if is_used_by_binary_crate(&fn_name, root) {
            skip_file(
                skipped,
                finding.file.clone(),
                binary_crate_usage_reason(&fn_name),
            );
            continue;
        }

        // Collect mod.rs files that re-export this function via `pub use`.
        // We'll generate ReexportRemoval fixes for these alongside the
        // visibility change.
        let reexport_files = symbol_surface.reexport_files.clone();

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

        // Generate visibility change for the source file.
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

        // Generate re-export removal fixes for parent mod.rs files.
        for reexport_file in &reexport_files {
            fixes.push(Fix {
                file: reexport_file.clone(),
                required_methods: vec![],
                required_registrations: vec![],
                insertions: vec![insertion(
                    InsertionKind::ReexportRemoval {
                        fn_name: fn_name.clone(),
                    },
                    AuditFinding::UnreferencedExport,
                    format!("Remove '{}' from pub use", fn_name),
                    format!(
                        "Remove re-export of '{}' from {} (no longer public)",
                        fn_name, reexport_file
                    ),
                )],
                applied: false,
            });
        }
    }
}

pub(crate) fn generate_duplicate_function_fixes(
    result: &CodeAuditResult,
    root: &Path,
    module_surfaces: &ModuleSurfaceIndex,
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
            generate_simple_duplicate_fixes(group, root, module_surfaces, fixes, skipped);
            continue;
        }

        let canonical_abs = root.join(&group.canonical_file);
        let ext = canonical_abs
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let language = Language::from_path(&canonical_abs);
        let use_extract_shared = matches!(language, Language::Php)
            && !crate::core::code_audit::is_test_path(&group.canonical_file);

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
            crate::core::extension::find_extension_for_file_ext(ext, "refactor")
        } else {
            None
        };

        let Some(manifest) = manifest else {
            generate_simple_duplicate_fixes(group, root, module_surfaces, fixes, skipped);
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
            "project_root": root.to_string_lossy(),
        });

        let Some(result_val) = crate::core::extension::run_refactor_script(&manifest, &extract_cmd)
        else {
            generate_simple_duplicate_fixes(group, root, module_surfaces, fixes, skipped);
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

        // Record files that the extension skipped due to body mismatch.
        if let Some(skipped_arr) = result_val
            .get("skipped_files")
            .and_then(|value| value.as_array())
        {
            for entry in skipped_arr {
                let file = entry
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = entry
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("body mismatch")
                    .to_string();
                if !file.is_empty() {
                    skipped.push(SkippedFile { file, reason });
                }
            }
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
                        // Validate that the line range actually contains the
                        // function declaration. If line numbers are stale or
                        // wrong, skip the entire edit for this file to avoid
                        // leaving orphaned trait-use statements (Bug 3).
                        let abs_path = root.join(&file);
                        let file_content = std::fs::read_to_string(&abs_path).unwrap_or_default();
                        let file_lines: Vec<&str> = file_content.lines().collect();
                        let start_idx = (start as usize).saturating_sub(1);
                        let end_idx = (end as usize).min(file_lines.len());

                        let fn_pattern = format!("function {}", group.function_name);
                        let lines_contain_fn = (start_idx..end_idx)
                            .any(|i| file_lines.get(i).is_some_and(|l| l.contains(&fn_pattern)));

                        if !lines_contain_fn {
                            skipped.push(SkippedFile {
                                file: file.clone(),
                                reason: format!(
                                    "Line range {}–{} does not contain `function {}` — \
                                     skipping trait extraction for this file",
                                    start, end, group.function_name
                                ),
                            });
                            // Skip the entire edit for this file — don't add
                            // trait-use or import without the removal.
                            continue;
                        }

                        insertions.push(function_removal(
                            AuditFinding::DuplicateFunction,
                            start as usize,
                            end as usize,
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
    module_surfaces: &ModuleSurfaceIndex,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    let Some(canonical_surface) = module_surfaces.get(&group.canonical_file) else {
        skipped.push(SkippedFile {
            file: group.canonical_file.clone(),
            reason: format!(
                "Missing module surface for canonical duplicate file '{}'",
                group.canonical_file
            ),
        });
        return;
    };

    for remove_file in &group.remove_from {
        // Integration test files (tests/) are separate compilation units —
        // they cannot import from each other via `use crate::`. Skip fixes
        // that would generate invalid cross-crate imports.
        if crate::core::code_audit::walker::is_test_path(remove_file)
            || crate::core::code_audit::walker::is_test_path(&group.canonical_file)
        {
            skip_file(
                skipped,
                remove_file.clone(),
                integration_test_duplicate_reason(&group.function_name),
            );
            continue;
        }

        let Some(remove_surface) = module_surfaces.get(remove_file) else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Missing module surface for duplicate file '{}'",
                    remove_file
                ),
            });
            continue;
        };

        if canonical_surface.is_api_barrel() || remove_surface.is_api_barrel() {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Duplicate spans API/barrel module surface (canonical: {}, duplicate: {})",
                    group.canonical_file, remove_file
                ),
            });
            continue;
        }

        if canonical_surface.owns_public_symbol(&group.function_name)
            && canonical_surface
                .symbol_surface(&group.function_name)
                .is_some_and(|surface| surface.has_external_usage(&group.canonical_file))
            && remove_surface.owns_public_symbol(&group.function_name)
        {
            skip_file(
                skipped,
                remove_file.clone(),
                externally_surfaced_duplicate_reason(&group.function_name),
            );
            continue;
        }

        let abs_path = root.join(remove_file.as_str());
        let ext = abs_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(content) => content,
            Err(_) => {
                skip_file(
                    skipped,
                    remove_file.clone(),
                    cannot_read_duplicate_reason(&group.function_name),
                );
                continue;
            }
        };

        let items = parse_items_for_dedup(ext, &content, remove_file);
        let Some(items) = items else {
            skip_file(
                skipped,
                remove_file.clone(),
                cannot_locate_boundaries_reason(&group.function_name, remove_file),
            );
            continue;
        };

        let Some(item) = find_parsed_item_by_name(&items, &group.function_name) else {
            skip_file(
                skipped,
                remove_file.clone(),
                function_not_found_reason(&group.function_name, remove_file),
            );
            continue;
        };

        let language = Language::from_path(&abs_path);
        let import_stmt =
            generate_duplicate_import(&group.canonical_file, &group.function_name, &language, root);

        let mut insertions = vec![function_removal(
            AuditFinding::DuplicateFunction,
            item.start_line,
            item.end_line,
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

fn is_used_by_binary_crate(fn_name: &str, root: &Path) -> bool {
    let word_re = match Regex::new(&format!(r"\b{}\b", regex::escape(fn_name))) {
        Ok(re) => re,
        Err(_) => return false,
    };

    let src = root.join("src");
    let main_rs = src.join("main.rs");
    if main_rs.exists() && file_contains_word(&main_rs, &word_re) {
        return true;
    }

    let lib_mods = read_mod_names(&src.join("lib.rs"));
    let main_mods = read_mod_names(&main_rs);

    for mod_name in main_mods.difference(&lib_mods) {
        let mod_dir = src.join(mod_name);
        if mod_dir.is_dir() && scan_dir_for_reference(&mod_dir, &word_re) {
            return true;
        }

        let mod_file = src.join(format!("{}.rs", mod_name));
        if mod_file.exists() && file_contains_word(&mod_file, &word_re) {
            return true;
        }
    }

    false
}

fn skip_file(skipped: &mut Vec<SkippedFile>, file: String, reason: String) {
    skipped.push(SkippedFile { file, reason });
}

fn external_usage_reason(fn_name: &str) -> String {
    format!(
        "Module surface shows '{}' still has external callers/importers/re-exports",
        fn_name
    )
}

fn binary_crate_usage_reason(fn_name: &str) -> String {
    format!(
        "Function '{}' is used by binary crate — cannot narrow visibility",
        fn_name
    )
}

fn integration_test_duplicate_reason(function_name: &str) -> String {
    format!(
        "Duplicate `{}` spans integration test files — test binaries cannot cross-import",
        function_name
    )
}

fn externally_surfaced_duplicate_reason(function_name: &str) -> String {
    format!(
        "Duplicate '{}' is externally surfaced in more than one module; keep ownership stable",
        function_name
    )
}

fn cannot_read_duplicate_reason(function_name: &str) -> String {
    format!("Cannot read file to remove duplicate `{}`", function_name)
}

fn cannot_locate_boundaries_reason(function_name: &str, file: &str) -> String {
    format!(
        "Cannot locate `{}` boundaries in {} — no grammar or extension available",
        function_name, file
    )
}

fn function_not_found_reason(function_name: &str, file: &str) -> String {
    format!(
        "Function `{}` not found by parser in {}",
        function_name, file
    )
}

fn file_contains_word(path: &Path, word_re: &Regex) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|content| word_re.is_match(&content))
}

fn read_mod_names(path: &Path) -> HashSet<String> {
    if path.exists() {
        std::fs::read_to_string(path)
            .ok()
            .map(|content| extract_mod_names(&content))
            .unwrap_or_default()
    } else {
        HashSet::new()
    }
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
    use crate::core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
        ..Default::default()
    };
    codebase_scan::any_file_matches(dir, &config, |path| {
        std::fs::read_to_string(path)
            .ok()
            .is_some_and(|content| word_re.is_match(&content))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_php_fqcn_with_namespace() {
        let content =
            "<?php\nnamespace SamplePlugin\\Abilities\\Fetch;\n\nclass FetchRssAbility {\n";
        assert_eq!(
            extract_php_fqcn(content),
            Some("SamplePlugin\\Abilities\\Fetch\\FetchRssAbility".to_string())
        );
    }

    #[test]
    fn test_extract_php_fqcn_abstract_class() {
        let content = "<?php\nnamespace SamplePlugin\\Core;\n\nabstract class BaseHandler {\n";
        assert_eq!(
            extract_php_fqcn(content),
            Some("SamplePlugin\\Core\\BaseHandler".to_string())
        );
    }

    #[test]
    fn test_extract_php_fqcn_no_namespace() {
        let content = "<?php\nclass SimpleClass {\n";
        assert_eq!(extract_php_fqcn(content), Some("SimpleClass".to_string()));
    }

    #[test]
    fn test_extract_php_fqcn_no_class() {
        let content = "<?php\nnamespace SamplePlugin;\n\nfunction helper() {}\n";
        assert_eq!(extract_php_fqcn(content), None);
    }

    #[test]
    fn test_generate_duplicate_import_rust() {
        let import = generate_duplicate_import(
            "src/core/engine/symbol_graph.rs",
            "parse_imports",
            &Language::Rust,
            Path::new("/tmp"),
        );
        assert_eq!(
            import,
            "use crate::core::engine::symbol_graph::parse_imports;"
        );
    }

    #[test]
    fn test_generate_duplicate_import_php() {
        use std::fs;
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();

        let php_dir = root.join("inc/Abilities/Fetch");
        fs::create_dir_all(&php_dir).unwrap();
        fs::write(
            php_dir.join("FetchRssAbility.php"),
            "<?php\nnamespace SamplePlugin\\Abilities\\Fetch;\n\nclass FetchRssAbility {\n    public function httpGet() {}\n}\n",
        ).unwrap();

        let import = generate_duplicate_import(
            "inc/Abilities/Fetch/FetchRssAbility.php",
            "httpGet",
            &Language::Php,
            root,
        );
        assert_eq!(
            import,
            "use SamplePlugin\\Abilities\\Fetch\\FetchRssAbility;"
        );
    }

    #[test]
    fn test_generate_duplicate_import_js() {
        let import = generate_duplicate_import(
            "src/utils/helpers.js",
            "formatDate",
            &Language::JavaScript,
            Path::new("/tmp"),
        );
        // JS imports derive the module name from the file path
        assert!(
            import.starts_with("import {"),
            "Expected JS import, got: {}",
            import
        );
        assert!(
            import.contains("helpers"),
            "Expected module name in import, got: {}",
            import
        );
    }
}
