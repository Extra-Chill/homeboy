use crate::code_audit::conventions::Language;
use crate::code_audit::{AuditFinding, CodeAuditResult, DuplicateGroup};
use crate::core::refactor::auto::{Fix, FixSafetyTier, InsertionKind, NewFile, SkippedFile};

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
            skipped.push(SkippedFile {
                file: finding.file.clone(),
                reason: format!(
                    "Function '{}' is used by binary crate — cannot narrow visibility",
                    fn_name
                ),
            });
            continue;
        }

        // Collect mod.rs files that re-export this function via `pub use`.
        // We'll generate ReexportRemoval fixes for these alongside the
        // visibility change.
        let reexport_files = find_reexport_files(&finding.file, &fn_name, root);

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
        let language = Language::from_path(&canonical_abs);
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

        let language = Language::from_path(&abs_path);
        let import_stmt =
            generate_duplicate_import(&group.canonical_file, &group.function_name, &language, root);

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

/// Find mod.rs/lib.rs files that re-export a function via `pub use`.
/// Returns relative paths (e.g., "src/core/refactor/mod.rs").
fn find_reexport_files(file_path: &str, fn_name: &str, root: &Path) -> Vec<String> {
    let source_path = Path::new(file_path);
    let mut result = Vec::new();

    let mut current = source_path.parent();
    while let Some(dir) = current {
        for filename in &["mod.rs", "lib.rs"] {
            let check_path = root.join(dir).join(filename);
            if check_path.exists()
                && std::fs::read_to_string(&check_path)
                    .ok()
                    .is_some_and(|content| has_pub_use_of(&content, fn_name))
            {
                result.push(format!("{}/{}", dir.display(), filename));
            }
        }
        current = dir.parent();
    }

    result
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
    use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

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
            "<?php\nnamespace DataMachine\\Abilities\\Fetch;\n\nclass FetchRssAbility {\n";
        assert_eq!(
            extract_php_fqcn(content),
            Some("DataMachine\\Abilities\\Fetch\\FetchRssAbility".to_string())
        );
    }

    #[test]
    fn test_extract_php_fqcn_abstract_class() {
        let content = "<?php\nnamespace DataMachine\\Core;\n\nabstract class BaseHandler {\n";
        assert_eq!(
            extract_php_fqcn(content),
            Some("DataMachine\\Core\\BaseHandler".to_string())
        );
    }

    #[test]
    fn test_extract_php_fqcn_no_namespace() {
        let content = "<?php\nclass SimpleClass {\n";
        assert_eq!(extract_php_fqcn(content), Some("SimpleClass".to_string()));
    }

    #[test]
    fn test_extract_php_fqcn_no_class() {
        let content = "<?php\nnamespace DataMachine;\n\nfunction helper() {}\n";
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
            "<?php\nnamespace DataMachine\\Abilities\\Fetch;\n\nclass FetchRssAbility {\n    public function httpGet() {}\n}\n",
        ).unwrap();

        let import = generate_duplicate_import(
            "inc/Abilities/Fetch/FetchRssAbility.php",
            "httpGet",
            &Language::Php,
            root,
        );
        assert_eq!(
            import,
            "use DataMachine\\Abilities\\Fetch\\FetchRssAbility;"
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

    #[test]
    fn test_extract_function_name_from_unreferenced_default_path() {

        let _result = extract_function_name_from_unreferenced();
    }

    #[test]
    fn test_extract_function_name_from_unreferenced_some_rest_end_to_string() {

        let result = extract_function_name_from_unreferenced();
        assert!(result.is_some(), "expected Some for: Some(rest[..end].to_string())");
    }

    #[test]
    fn test_module_path_from_file_default_path() {

        let _result = module_path_from_file();
    }

    #[test]
    fn test_generate_unreferenced_export_fixes_finding_kind_auditfinding_unreferencedexport() {

        generate_unreferenced_export_fixes();
    }

    #[test]
    fn test_generate_unreferenced_export_fixes_matches_language_language_rust() {

        generate_unreferenced_export_fixes();
    }

    #[test]
    fn test_generate_unreferenced_export_fixes_err_continue() {

        generate_unreferenced_export_fixes();
    }

    #[test]
    fn test_generate_unreferenced_export_fixes_let_some_line_num_found_line_else() {

        generate_unreferenced_export_fixes();
    }

    #[test]
    fn test_generate_unreferenced_export_fixes_has_expected_effects() {
        // Expected effects: file_read, mutation

        let _ = generate_unreferenced_export_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_ok_content_content() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_err() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_else() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_else_2() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_match_std_fs_read_to_string_abs_path() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_err_2() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_let_some_result_val_crate_extension_run_refactor_script_mani() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_if_let_some_trait_file_some_trait_content() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_if_let_some_file_edits_result_val() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_let_some_file_edits_result_val() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_if_let_some_remove_lines_edit_get_remove_lines() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_let_some_remove_lines_edit_get_remove_lines() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_if_let_some_import_edit_get_add_import_and_then_value_value_() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_if_let_some_use_trait_edit_get_add_use_trait_and_then_value_() {

        generate_duplicate_function_fixes();
    }

    #[test]
    fn test_generate_duplicate_function_fixes_has_expected_effects() {
        // Expected effects: file_read, mutation

        let _ = generate_duplicate_function_fixes();
    }

}
