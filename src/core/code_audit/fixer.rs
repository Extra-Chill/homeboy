//! Auto-fix engine — generate and apply stubs for convention outliers.
//!
//! Given an audit result, reads conforming files to extract full method
//! signatures, then generates stub insertions for outlier files.
//!
//! Two modes:
//! - Dry run (default): returns fixes without modifying files
//! - Write mode: applies fixes to disk

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

use super::conventions::{DeviationKind, Language};
use super::{duplication, CodeAuditResult};

/// A planned fix for a single file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    /// Relative path to the file being fixed.
    pub file: String,
    /// What will be inserted.
    pub insertions: Vec<Insertion>,
    /// Whether the fix was applied to disk.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub applied: bool,
}

/// A single insertion into a file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Insertion {
    /// What kind of fix.
    pub kind: InsertionKind,
    /// The code to insert.
    pub code: String,
    /// Human-readable description.
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionKind {
    MethodStub,
    RegistrationStub,
    ConstructorWithRegistration,
    /// Add a missing import/use statement at the top of the file.
    ImportAdd,
    /// Remove a function definition (lines start_line..=end_line) and replace with an import.
    FunctionRemoval {
        /// 1-indexed start line (includes doc comments and attributes).
        start_line: usize,
        /// 1-indexed end line (inclusive).
        end_line: usize,
    },
    /// Insert a trait `use` statement inside a class body (PHP `use TraitName;`).
    /// Language-agnostic: for Rust this could be a trait impl, for JS a mixin.
    /// The code is inserted after the class/struct opening brace.
    TraitUse,
}

/// A file that was skipped by the fixer with a reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkippedFile {
    /// Relative file path.
    pub file: String,
    /// Why it was skipped.
    pub reason: String,
}

/// A new file to create (e.g., a trait file for extracted duplicates).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewFile {
    /// Relative path for the new file.
    pub file: String,
    /// Content to write.
    pub content: String,
    /// Human-readable description.
    pub description: String,
    /// Whether the file was written to disk.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub written: bool,
}

/// Result of running the fixer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixResult {
    pub fixes: Vec<Fix>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub new_files: Vec<NewFile>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub skipped: Vec<SkippedFile>,
    pub total_insertions: usize,
    pub files_modified: usize,
}

// ============================================================================
// Signature Extraction
// ============================================================================

/// Full method signature extracted from a conforming file.
#[derive(Debug, Clone)]
pub(super) struct MethodSignature {
    /// Method name.
    pub(super) name: String,
    /// Full signature line (e.g., "public function execute(array $config): array").
    pub(super) signature: String,
    /// The language this was extracted from.
    #[allow(dead_code)]
    pub(super) language: Language,
}

/// Extract full method signatures from a source file.
pub(super) fn extract_signatures(content: &str, language: &Language) -> Vec<MethodSignature> {
    match language {
        Language::Php => extract_php_signatures(content),
        Language::Rust => extract_rust_signatures(content),
        Language::JavaScript | Language::TypeScript => extract_js_signatures(content),
        Language::Unknown => vec![],
    }
}

fn extract_php_signatures(content: &str) -> Vec<MethodSignature> {
    let re = Regex::new(
        r"(?m)^\s*((?:public|protected|private)\s+(?:static\s+)?function\s+(\w+)\s*\([^)]*\)(?:\s*:\s*[\w\\|?]+)?)",
    )
    .unwrap();

    re.captures_iter(content)
        .map(|cap| MethodSignature {
            name: cap[2].to_string(),
            signature: cap[1].trim().to_string(),
            language: Language::Php,
        })
        .collect()
}

fn extract_rust_signatures(content: &str) -> Vec<MethodSignature> {
    let re = Regex::new(
        r"(?m)^\s*(pub(?:\(crate\))?\s+(?:async\s+)?fn\s+(\w+)\s*\([^)]*\)(?:\s*->\s*[^\{]+)?)",
    )
    .unwrap();

    re.captures_iter(content)
        .map(|cap| MethodSignature {
            name: cap[2].to_string(),
            signature: cap[1].trim().to_string(),
            language: Language::Rust,
        })
        .collect()
}

fn extract_js_signatures(content: &str) -> Vec<MethodSignature> {
    // Named function declarations
    let fn_re =
        Regex::new(r"(?m)^\s*((?:export\s+)?(?:async\s+)?function\s+(\w+)\s*\([^)]*\))").unwrap();
    // Class methods
    let method_re = Regex::new(r"(?m)^\s+((?:async\s+)?(\w+)\s*\([^)]*\))\s*\{").unwrap();

    let mut sigs: Vec<MethodSignature> = fn_re
        .captures_iter(content)
        .map(|cap| MethodSignature {
            name: cap[2].to_string(),
            signature: cap[1].trim().to_string(),
            language: Language::JavaScript,
        })
        .collect();

    let skip = ["if", "for", "while", "switch", "catch", "return"];
    for cap in method_re.captures_iter(content) {
        let name = cap[2].to_string();
        if !skip.contains(&name.as_str()) && !sigs.iter().any(|s| s.name == name) {
            sigs.push(MethodSignature {
                name,
                signature: cap[1].trim().to_string(),
                language: Language::JavaScript,
            });
        }
    }

    sigs
}

// ============================================================================
// Stub Generation
// ============================================================================

/// Generate a stub body for a method based on language.
fn stub_body(method_name: &str, language: &Language) -> String {
    match language {
        Language::Php => {
            format!(
                "        throw new \\RuntimeException('Not implemented: {}');",
                method_name
            )
        }
        Language::Rust => {
            format!("        todo!(\"{}\")", method_name)
        }
        Language::JavaScript | Language::TypeScript => {
            format!(
                "        throw new Error('Not implemented: {}');",
                method_name
            )
        }
        Language::Unknown => String::new(),
    }
}

/// Generate a method stub from a signature.
fn generate_method_stub(sig: &MethodSignature) -> String {
    let body = stub_body(&sig.name, &sig.language);
    match sig.language {
        Language::Php => {
            format!("\n    {} {{\n{}\n    }}\n", sig.signature, body)
        }
        Language::Rust => {
            format!("\n    {} {{\n{}\n    }}\n", sig.signature, body)
        }
        Language::JavaScript | Language::TypeScript => {
            format!("\n    {} {{\n{}\n    }}\n", sig.signature, body)
        }
        Language::Unknown => String::new(),
    }
}

// ============================================================================
// Import Generation
// ============================================================================

/// Generate the import statement line for a given import path.
///
/// Language-aware: `use X;` for Rust/PHP, `import X from 'X';` for JS/TS.
fn generate_import_statement(import_path: &str, language: &Language) -> String {
    match language {
        Language::Rust => format!("use {};", import_path),
        Language::Php => format!("use {};", import_path),
        Language::JavaScript | Language::TypeScript => {
            // Extract the last segment as the name
            let name = import_path
                .rsplit("::")
                .next()
                .or_else(|| import_path.rsplit('/').next())
                .unwrap_or(import_path);
            format!("import {{ {} }} from '{}';", name, import_path)
        }
        Language::Unknown => format!("use {};", import_path),
    }
}

/// Insert an import statement into file content at the correct location.
///
/// Finds the last existing import/use line and inserts after it.
/// If no imports exist, inserts after the first non-comment, non-blank line
/// (e.g., after `<?php` or after extension-level attributes).
fn insert_import(content: &str, import_line: &str, language: &Language) -> String {
    let lines: Vec<&str> = content.lines().collect();

    // Find the last import/use line
    let import_prefix = match language {
        Language::Rust => "use ",
        Language::Php => "use ",
        Language::JavaScript | Language::TypeScript => "import ",
        Language::Unknown => "use ",
    };

    let mut last_import_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with(import_prefix)
            || (trimmed.starts_with("use ") && *language == Language::Rust)
        {
            last_import_idx = Some(i);
        }
    }

    let insert_after = if let Some(idx) = last_import_idx {
        idx
    } else {
        // No existing imports — insert after first non-blank, non-comment line
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
        // Insert before first_code (add a blank line separator)
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

    // Preserve trailing newline behavior
    if !content.ends_with('\n') {
        result.pop();
    }

    result
}

/// Generate a registration stub for PHP (add_action/add_filter in __construct).
fn generate_registration_stub(hook_name: &str) -> String {
    // The hook name from the audit is the first arg of add_action
    // We need to generate: add_action('hook_name', [$this, 'methodName']);
    // Use a generic callback name based on the hook
    let callback = hook_name
        .strip_prefix("wp_")
        .or_else(|| hook_name.strip_prefix("datamachine_"))
        .unwrap_or(hook_name);

    format!(
        "        add_action('{}', [$this, '{}']);",
        hook_name, callback
    )
}

// ============================================================================
// Fix Generation
// ============================================================================

/// Build a signature map from conforming files for a convention.
fn build_signature_map(
    conforming_files: &[String],
    root: &Path,
) -> HashMap<String, MethodSignature> {
    let mut sig_map: HashMap<String, MethodSignature> = HashMap::new();

    for rel_path in conforming_files {
        let abs_path = root.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&abs_path) {
            let language = abs_path
                .extension()
                .and_then(|e| e.to_str())
                .map(Language::from_extension)
                .unwrap_or(Language::Unknown);

            for sig in extract_signatures(&content, &language) {
                // Keep the first signature found (from the first conforming file)
                sig_map.entry(sig.name.clone()).or_insert(sig);
            }
        }
    }

    sig_map
}

/// Detect the language of a file from its path.
fn detect_language(path: &Path) -> Language {
    path.extension()
        .and_then(|e| e.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::Unknown)
}

/// Check if a file has a __construct method.
fn file_has_constructor(content: &str, language: &Language) -> bool {
    match language {
        Language::Php => content.contains("function __construct"),
        Language::Rust => content.contains("fn new("),
        Language::JavaScript | Language::TypeScript => content.contains("constructor("),
        Language::Unknown => false,
    }
}

/// Generate fixes for a single audit result.
///
/// Smart filtering rules:
/// 1. Skip fragmented conventions (confidence < 50%) — too weak to trust
/// 2. Skip files that don't match the naming pattern of their siblings
///    (e.g., `FlowHelpers.php` among `*Ability.php` files)
/// 3. Only add registration stubs when the file already has the callback
///    method, or when adding to an existing constructor
pub fn generate_fixes(result: &CodeAuditResult, root: &Path) -> FixResult {
    let mut fixes = Vec::new();
    let mut skipped = Vec::new();

    for conv_report in &result.conventions {
        if conv_report.outliers.is_empty() {
            continue;
        }

        // Filter 1: Skip fragmented conventions — too weak to generate fixes
        if conv_report.confidence < 0.5 {
            for outlier in &conv_report.outliers {
                skipped.push(SkippedFile {
                    file: outlier.file.clone(),
                    reason: format!(
                        "Convention '{}' confidence too low ({:.0}%) — needs manual review",
                        conv_report.name,
                        conv_report.confidence * 100.0
                    ),
                });
            }
            continue;
        }

        // Filter 2: Detect naming pattern from conforming files
        let naming_suffix = detect_naming_suffix(&conv_report.conforming);

        // Build signature map from conforming files
        let sig_map = build_signature_map(&conv_report.conforming, root);

        for outlier in &conv_report.outliers {
            // Filter 2 check: skip files that don't match the naming pattern
            if let Some(ref suffix) = naming_suffix {
                let file_stem = Path::new(&outlier.file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !suffix_matches(&file_stem, suffix) {
                    skipped.push(SkippedFile {
                        file: outlier.file.clone(),
                        reason: format!(
                            "Name doesn't match convention pattern '*{}' — likely a utility/helper, needs manual refactoring",
                            suffix
                        ),
                    });
                    continue;
                }
            }

            let mut insertions = Vec::new();
            let abs_path = root.join(&outlier.file);
            let language = detect_language(&abs_path);
            let content = std::fs::read_to_string(&abs_path).unwrap_or_default();
            let has_constructor = file_has_constructor(&content, &language);

            // First pass: collect missing methods and missing registrations
            let mut missing_methods: Vec<&str> = Vec::new();
            let mut missing_registrations: Vec<&str> = Vec::new();
            let mut missing_imports: Vec<&str> = Vec::new();
            let mut needs_constructor = false;

            for deviation in &outlier.deviations {
                match deviation.kind {
                    DeviationKind::MissingMethod => {
                        let method_name = deviation
                            .description
                            .strip_prefix("Missing method: ")
                            .unwrap_or(&deviation.description);

                        // Filter 3: Skip short method names (i18n noise like __)
                        if method_name.len() < 3 {
                            continue;
                        }

                        if method_name == "__construct"
                            || method_name == "new"
                            || method_name == "constructor"
                        {
                            needs_constructor = true;
                        } else {
                            missing_methods.push(method_name);
                        }
                    }
                    DeviationKind::MissingRegistration => {
                        let hook_name = deviation
                            .description
                            .strip_prefix("Missing registration: ")
                            .unwrap_or(&deviation.description);
                        missing_registrations.push(hook_name);
                    }
                    DeviationKind::MissingImport => {
                        let import_path = deviation
                            .description
                            .strip_prefix("Missing import: ")
                            .unwrap_or(&deviation.description);
                        missing_imports.push(import_path);
                    }
                    _ => {}
                }
            }

            // Second pass: generate insertions

            // Handle missing imports: generate use statements
            for import_path in &missing_imports {
                let use_stmt = generate_import_statement(import_path, &language);
                insertions.push(Insertion {
                    kind: InsertionKind::ImportAdd,
                    code: use_stmt,
                    description: format!("Add missing import: {}", import_path),
                });
            }

            // Handle registrations: either inject into existing constructor, or create new one
            if !missing_registrations.is_empty() && language == Language::Php {
                if has_constructor && !needs_constructor {
                    // Inject registrations into existing __construct
                    for hook_name in &missing_registrations {
                        insertions.push(Insertion {
                            kind: InsertionKind::RegistrationStub,
                            code: generate_registration_stub(hook_name),
                            description: format!("Add {} registration in __construct()", hook_name),
                        });
                    }
                } else {
                    // Create new __construct with all registrations inside
                    let reg_lines: String = missing_registrations
                        .iter()
                        .map(|h| generate_registration_stub(h))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let construct_code = format!(
                        "\n    public function __construct() {{\n{}\n    }}\n",
                        reg_lines
                    );
                    insertions.push(Insertion {
                        kind: InsertionKind::ConstructorWithRegistration,
                        code: construct_code,
                        description: format!(
                            "Add __construct() with {} registration(s)",
                            missing_registrations.len()
                        ),
                    });
                    // Mark constructor as handled so we don't also add a bare stub
                    needs_constructor = false;
                }
            }

            // If constructor is still needed (missing method, no registrations to bundle)
            if needs_constructor {
                let constructor_name = match language {
                    Language::Php => "__construct",
                    Language::Rust => "new",
                    Language::JavaScript | Language::TypeScript => "constructor",
                    Language::Unknown => "__construct",
                };
                if let Some(sig) = sig_map.get(constructor_name) {
                    insertions.push(Insertion {
                        kind: InsertionKind::MethodStub,
                        code: generate_method_stub(sig),
                        description: format!(
                            "Add {}() stub to match {} convention",
                            constructor_name, conv_report.name
                        ),
                    });
                } else {
                    let fallback_sig = generate_fallback_signature(constructor_name, &language);
                    insertions.push(Insertion {
                        kind: InsertionKind::MethodStub,
                        code: generate_method_stub(&fallback_sig),
                        description: format!(
                            "Add {}() stub to match {} convention (signature inferred)",
                            constructor_name, conv_report.name
                        ),
                    });
                }
            }

            // Generate method stubs for all other missing methods
            for method_name in &missing_methods {
                if let Some(sig) = sig_map.get(*method_name) {
                    insertions.push(Insertion {
                        kind: InsertionKind::MethodStub,
                        code: generate_method_stub(sig),
                        description: format!(
                            "Add {}() stub to match {} convention",
                            method_name, conv_report.name
                        ),
                    });
                } else {
                    let fallback_sig = generate_fallback_signature(method_name, &language);
                    insertions.push(Insertion {
                        kind: InsertionKind::MethodStub,
                        code: generate_method_stub(&fallback_sig),
                        description: format!(
                            "Add {}() stub to match {} convention (signature inferred)",
                            method_name, conv_report.name
                        ),
                    });
                }
            }

            if !insertions.is_empty() {
                fixes.push(Fix {
                    file: outlier.file.clone(),
                    insertions,
                    applied: false,
                });
            }
        }
    }

    // Phase 1b: Missing test files from test_coverage findings.
    // These are mechanical and safe to scaffold.
    let mut new_files: Vec<NewFile> = Vec::new();
    for finding in &result.findings {
        if finding.kind != DeviationKind::MissingTestFile {
            continue;
        }

        let Some(test_file) = extract_expected_test_path(&finding.description) else {
            continue;
        };

        let abs_test_path = root.join(&test_file);
        if abs_test_path.exists() || new_files.iter().any(|nf| nf.file == test_file) {
            continue;
        }

        let content = generate_test_file_stub(&test_file, &finding.file);
        new_files.push(NewFile {
            file: test_file,
            content,
            description: format!("Create missing test file for '{}'", finding.file),
            written: false,
        });
    }

    // Phase 2: Duplication fixes — extract shared code via extension protocol

    /// Minimum number of files (including canonical) before extracting to shared code.
    /// Groups with fewer files are reported as findings but not auto-fixed —
    /// the overhead of a trait/module for 2-3 files isn't worth it.
    const MIN_EXTRACT_GROUP_SIZE: usize = 4;

    /// Function names that shouldn't be extracted to traits/shared modules.
    /// These are typically boilerplate that's better handled by inheritance
    /// or factory patterns, not trait extraction.
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
        let group_size = 1 + group.remove_from.len(); // canonical + duplicates

        // Skip small groups — not worth extracting to shared code
        if group_size < MIN_EXTRACT_GROUP_SIZE {
            continue;
        }

        // Skip constructors and test lifecycle methods
        if SKIP_EXTRACT_NAMES.contains(&group.function_name.as_str()) {
            continue;
        }

        let canonical_abs = root.join(&group.canonical_file);
        let ext = canonical_abs
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language = detect_language(&canonical_abs);

        // Only use extract_shared for PHP class methods (not tests, not JS/JSX).
        let is_test_file = group.canonical_file.contains("/tests/")
            || group.canonical_file.contains("/Tests/")
            || group.canonical_file.starts_with("tests/");
        let use_extract_shared = matches!(language, Language::Php) && !is_test_file;

        let ext_manifest = crate::extension::find_extension_for_file_ext(ext, "refactor");

        let canonical_content = match std::fs::read_to_string(&canonical_abs) {
            Ok(c) => c,
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
            ext_manifest
        } else {
            None
        };

        let Some(manifest) = manifest else {
            // Fall back to simple remove+import for languages without extract_shared
            generate_simple_duplicate_fixes(group, root, &mut fixes, &mut skipped);
            continue;
        };

        // Read all duplicate file contents
        let mut file_entries = Vec::new();
        let mut any_read_failure = false;
        for remove_file in &group.remove_from {
            let abs_path = root.join(remove_file);
            match std::fs::read_to_string(&abs_path) {
                Ok(c) => {
                    file_entries.push(serde_json::json!({
                        "path": remove_file,
                        "content": c,
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

        // Collect all file paths for common ancestor namespace computation
        let mut all_paths: Vec<&str> = vec![group.canonical_file.as_str()];
        all_paths.extend(group.remove_from.iter().map(|s| s.as_str()));

        // Call the extension's extract_shared command
        let extract_cmd = serde_json::json!({
            "command": "extract_shared",
            "function_name": group.function_name,
            "canonical_file": group.canonical_file,
            "canonical_content": canonical_content,
            "files": file_entries,
            "all_file_paths": all_paths,
        });

        let extract_result = crate::extension::run_refactor_script(&manifest, &extract_cmd);

        let Some(result_val) = extract_result else {
            // Extension doesn't support extract_shared, fall back
            generate_simple_duplicate_fixes(group, root, &mut fixes, &mut skipped);
            continue;
        };

        // Check for error or skip
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
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let reason = result_val
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("extension decided to skip");
            skipped.push(SkippedFile {
                file: group.canonical_file.clone(),
                reason: format!("Skipped `{}`: {}", group.function_name, reason),
            });
            continue;
        }

        // Parse the trait/shared file info
        if let (Some(trait_file), Some(trait_content)) = (
            result_val.get("trait_file").and_then(|v| v.as_str()),
            result_val.get("trait_content").and_then(|v| v.as_str()),
        ) {
            // Only add the new file once (avoid duplicates from multiple groups)
            if !new_files.iter().any(|nf| nf.file == trait_file) {
                let trait_name = result_val
                    .get("trait_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("SharedTrait");
                new_files.push(NewFile {
                    file: trait_file.to_string(),
                    content: trait_content.to_string(),
                    description: format!(
                        "Create trait `{}` for shared `{}` method",
                        trait_name, group.function_name
                    ),
                    written: false,
                });
            }
        }

        // Parse the per-file edits
        if let Some(file_edits) = result_val.get("file_edits").and_then(|v| v.as_array()) {
            for edit in file_edits {
                let file = match edit.get("file").and_then(|v| v.as_str()) {
                    Some(f) => f.to_string(),
                    None => continue,
                };

                let mut insertions = Vec::new();

                // Function removal
                if let Some(rl) = edit.get("remove_lines") {
                    if let (Some(start), Some(end)) = (
                        rl.get("start_line").and_then(|v| v.as_u64()),
                        rl.get("end_line").and_then(|v| v.as_u64()),
                    ) {
                        insertions.push(Insertion {
                            kind: InsertionKind::FunctionRemoval {
                                start_line: start as usize,
                                end_line: end as usize,
                            },
                            code: String::new(),
                            description: format!(
                                "Remove duplicate `{}` (extracted to shared trait)",
                                group.function_name
                            ),
                        });
                    }
                }

                // Import statement (namespace-level use)
                if let Some(import) = edit.get("add_import").and_then(|v| v.as_str()) {
                    insertions.push(Insertion {
                        kind: InsertionKind::ImportAdd,
                        code: import.to_string(),
                        description: format!("Import shared trait for `{}`", group.function_name),
                    });
                }

                // Trait use statement (inside class body)
                if let Some(use_trait) = edit.get("add_use_trait").and_then(|v| v.as_str()) {
                    insertions.push(Insertion {
                        kind: InsertionKind::TraitUse,
                        code: use_trait.to_string(),
                        description: format!("Use shared trait for `{}`", group.function_name),
                    });
                }

                if !insertions.is_empty() {
                    fixes.push(Fix {
                        file,
                        insertions,
                        applied: false,
                    });
                }
            }
        }
    }

    // Phase 2 complete — merge and return
    // Merge fixes that target the same file.
    //
    // Multiple phases (convention fixes, duplication fixes) or multiple
    // duplicate groups can produce separate `Fix` objects for the same file.
    // If applied independently, the second fix uses stale line numbers because
    // the file was already modified by the first.  Merging into a single `Fix`
    // per file ensures `apply_insertions_to_content()` sees *all* removals at
    // once and can sort them in reverse order so line numbers stay valid.
    let fixes = merge_fixes_per_file(fixes);

    let total_insertions: usize = fixes.iter().map(|f| f.insertions.len()).sum();
    let files_modified = fixes.len();

    FixResult {
        fixes,
        new_files,
        skipped,
        total_insertions,
        files_modified,
    }
}

/// Extract the expected test file path from a MissingTestFile description.
///
/// Example description:
/// "No test file found (expected 'tests/utils/token_test.rs') and no inline tests"
fn extract_expected_test_path(description: &str) -> Option<String> {
    let needle = "expected '";
    let start = description.find(needle)? + needle.len();
    let rest = &description[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Generate a minimal test file stub for the given test file path.
///
/// Keeps stubs intentionally simple and compiling. This unblocks CI/audit
/// and creates an explicit place for real tests to be added.
fn generate_test_file_stub(test_file: &str, source_file: &str) -> String {
    let ext = Path::new(test_file)
        .extension()
        .and_then(|e| e.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::Unknown);

    match ext {
        Language::Rust => {
            let name = Path::new(source_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("module")
                .replace('-', "_");
            format!(
                "// Auto-generated by `homeboy audit --fix`\n// Source: {}\n\n#[test]\nfn test_{}_placeholder() {{\n    assert!(true, \"TODO: add real tests\");\n}}\n",
                source_file, name
            )
        }
        Language::Php => {
            format!(
                "<?php\n\n// Auto-generated by `homeboy audit --fix`\n// Source: {}\n\nuse PHPUnit\\Framework\\TestCase;\n\nfinal class GeneratedPlaceholderTest extends TestCase {{\n    public function test_placeholder(): void {{\n        $this->assertTrue(true);\n    }}\n}}\n",
                source_file
            )
        }
        _ => format!(
            "// Auto-generated by `homeboy audit --fix`\n// Source: {}\n// TODO: add tests\n",
            source_file
        ),
    }
}

/// Fallback duplicate fix for languages without `extract_shared` support.
///
/// Uses `parse_items` to find function boundaries, removes the duplicate,
/// and adds a simple import statement. This works for Rust (standalone fns)
/// but is less ideal for OOP languages where the function is a class method.
fn generate_simple_duplicate_fixes(
    group: &duplication::DuplicateGroup,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    for remove_file in &group.remove_from {
        let abs_path = root.join(remove_file.as_str());
        let ext = abs_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let ext_manifest = crate::extension::find_extension_for_file_ext(ext, "refactor");
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
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

        let Some(manifest) = ext_manifest else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "No refactor extension for .{} files — cannot locate `{}` boundaries",
                    ext, group.function_name
                ),
            });
            continue;
        };

        // Call parse_items to find the function boundaries
        let parse_cmd = serde_json::json!({
            "command": "parse_items",
            "file_path": remove_file,
            "content": content,
            "items": [group.function_name],
        });

        let parsed: Option<Vec<crate::extension::ParsedItem>> =
            crate::extension::run_refactor_script(&manifest, &parse_cmd)
                .and_then(|v| v.get("items").cloned())
                .and_then(|v| serde_json::from_value(v).ok());

        let Some(items) = parsed else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Extension could not parse `{}` boundaries in {}",
                    group.function_name, remove_file
                ),
            });
            continue;
        };

        let Some(item) = items.iter().find(|i| i.name == group.function_name) else {
            skipped.push(SkippedFile {
                file: remove_file.clone(),
                reason: format!(
                    "Function `{}` not found by parser in {}",
                    group.function_name, remove_file
                ),
            });
            continue;
        };

        // Build the import path from the canonical file
        let import_path = module_path_from_file(&group.canonical_file);
        let import_stmt = match ext {
            "rs" => format!("use crate::{}::{};", import_path, group.function_name),
            _ => format!(
                "import {{ {} }} from '{}';",
                group.function_name, import_path
            ),
        };

        let mut insertions = vec![Insertion {
            kind: InsertionKind::FunctionRemoval {
                start_line: item.start_line,
                end_line: item.end_line,
            },
            code: String::new(),
            description: format!(
                "Remove duplicate `{}` (canonical copy in {})",
                group.function_name, group.canonical_file
            ),
        }];

        // Only add the import if the file doesn't already have it
        if !content.contains(&import_stmt) {
            insertions.push(Insertion {
                kind: InsertionKind::ImportAdd,
                code: import_stmt,
                description: format!("Import `{}` from canonical location", group.function_name),
            });
        }

        fixes.push(Fix {
            file: remove_file.clone(),
            insertions,
            applied: false,
        });
    }
}

/// Merge multiple `Fix` objects that target the same file into one.
///
/// Preserves insertion order within each original `Fix`, appending later
/// fixes' insertions after earlier ones.  The resulting vec has at most one
/// `Fix` per unique file path.
fn merge_fixes_per_file(fixes: Vec<Fix>) -> Vec<Fix> {
    let mut map: std::collections::HashMap<String, Fix> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for fix in fixes {
        if let Some(existing) = map.get_mut(&fix.file) {
            existing.insertions.extend(fix.insertions);
        } else {
            order.push(fix.file.clone());
            map.insert(fix.file.clone(), fix);
        }
    }

    // Preserve original encounter order
    order.into_iter().filter_map(|f| map.remove(&f)).collect()
}

/// Convert a relative file path to a Rust module path.
///
/// `src/core/update_check.rs` → `core::update_check`
/// `src/utils/mod.rs` → `utils`
fn module_path_from_file(file_path: &str) -> String {
    let p = file_path.strip_prefix("src/").unwrap_or(file_path);
    let p = p.strip_suffix(".rs").unwrap_or(p);
    let p = p.strip_suffix("/mod").unwrap_or(p);
    p.replace('/', "::")
}

/// Detect the common naming suffix among conforming files.
///
/// If 4 out of 5 conforming files end in "Ability.php", returns Some("Ability").
/// If no clear pattern, returns None.
fn detect_naming_suffix(conforming: &[String]) -> Option<String> {
    if conforming.len() < 2 {
        return None;
    }

    // Extract file stems (without extension)
    let stems: Vec<String> = conforming
        .iter()
        .filter_map(|f| {
            Path::new(f)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();

    if stems.len() < 2 {
        return None;
    }

    // Try common suffixes by checking the longest common suffix among all stems
    // Start from the end of each stem and find the shared suffix
    let mut suffix_counts: HashMap<String, usize> = HashMap::new();

    for stem in &stems {
        // Extract suffix: last uppercase-start word (e.g., "Ability" from "FlowAbility")
        if let Some(suffix) = extract_class_suffix(stem) {
            *suffix_counts.entry(suffix).or_insert(0) += 1;
        }
    }

    // Find suffix that appears in ≥ 60% of conforming files
    let threshold = (stems.len() as f32 * 0.6).ceil() as usize;
    suffix_counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .max_by_key(|(_, count)| *count)
        .map(|(suffix, _)| suffix)
}

/// Extract the class-style suffix from a PascalCase name.
///
/// "FlowAbility" → "Ability"
/// "CreateFlowAbility" → "Ability"
/// "FlowHelpers" → "Helpers"
/// "step_a" → None (not PascalCase)
fn extract_class_suffix(name: &str) -> Option<String> {
    // Find the last uppercase letter that starts a "word"
    let chars: Vec<char> = name.chars().collect();
    let mut last_upper_start = None;

    for (i, ch) in chars.iter().enumerate() {
        if ch.is_uppercase() && i > 0 {
            last_upper_start = Some(i);
        }
    }

    last_upper_start.map(|i| chars[i..].iter().collect())
}

/// Check if a file stem matches a naming suffix, with plural tolerance.
///
/// "GitHubAbilities" matches suffix "Ability" (plural of Ability = Abilities)
/// "CreateFlowAbility" matches suffix "Ability" (exact)
/// "FlowHelpers" does NOT match suffix "Ability"
fn suffix_matches(file_stem: &str, suffix: &str) -> bool {
    if file_stem.ends_with(suffix) {
        return true;
    }

    // Try plural forms: Ability/Abilities, Test/Tests, Provider/Providers
    let plural_suffix = pluralize(suffix);
    if file_stem.ends_with(&plural_suffix) {
        return true;
    }

    // Try singular: if suffix is already plural, check if file matches singular
    if let Some(singular) = singularize(suffix) {
        if file_stem.ends_with(&singular) {
            return true;
        }
    }

    false
}

/// Simple English pluralization for class suffixes.
fn pluralize(word: &str) -> String {
    if word.ends_with('y')
        && !word.ends_with("ey")
        && !word.ends_with("ay")
        && !word.ends_with("oy")
    {
        // Ability → Abilities, Entity → Entities
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        format!("{}es", word)
    } else {
        format!("{}s", word)
    }
}

/// Simple English singularization for class suffixes.
fn singularize(word: &str) -> Option<String> {
    if word.ends_with("ies") && word.len() > 3 {
        // Abilities → Ability
        Some(format!("{}y", &word[..word.len() - 3]))
    } else if word.ends_with("ses")
        || word.ends_with("xes")
        || word.ends_with("ches")
        || word.ends_with("shes")
    {
        Some(word[..word.len() - 2].to_string())
    } else if word.ends_with('s') && !word.ends_with("ss") && word.len() > 1 {
        // Tests → Test, Providers → Provider
        Some(word[..word.len() - 1].to_string())
    } else {
        None
    }
}

/// Generate a fallback signature when no conforming file has the method.
fn generate_fallback_signature(method_name: &str, language: &Language) -> MethodSignature {
    let signature = match language {
        Language::Php => format!("public function {}()", method_name),
        Language::Rust => format!("pub fn {}()", method_name),
        Language::JavaScript | Language::TypeScript => format!("{}()", method_name),
        Language::Unknown => format!("{}()", method_name),
    };

    MethodSignature {
        name: method_name.to_string(),
        signature,
        language: language.clone(),
    }
}

// ============================================================================
// File Modification
// ============================================================================

/// Apply fixes to files on disk.
pub fn apply_fixes(fixes: &mut [Fix], root: &Path) -> usize {
    let mut applied_count = 0;

    for fix in fixes.iter_mut() {
        let abs_path = root.join(&fix.file);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                log_status!("fix", "Failed to read {}: {}", fix.file, e);
                continue;
            }
        };

        let language = detect_language(&abs_path);
        let modified = apply_insertions_to_content(&content, &fix.insertions, &language);

        if modified != content {
            match std::fs::write(&abs_path, &modified) {
                Ok(_) => {
                    fix.applied = true;
                    applied_count += 1;
                    log_status!(
                        "fix",
                        "Applied {} fix(es) to {}",
                        fix.insertions.len(),
                        fix.file
                    );
                }
                Err(e) => {
                    log_status!("fix", "Failed to write {}: {}", fix.file, e);
                }
            }
        }
    }

    applied_count
}

/// Write new files generated by the fixer (e.g., trait files for extracted duplicates).
///
/// Returns the number of files successfully created.
pub fn apply_new_files(new_files: &mut [NewFile], root: &Path) -> usize {
    let mut created = 0;

    for nf in new_files.iter_mut() {
        let abs_path = root.join(&nf.file);

        // Create parent directories if needed
        if let Some(parent) = abs_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    log_status!("fix", "Failed to create directory for {}: {}", nf.file, e);
                    continue;
                }
            }
        }

        // Don't overwrite existing files
        if abs_path.exists() {
            log_status!("fix", "Skipping {} — file already exists", nf.file);
            continue;
        }

        match std::fs::write(&abs_path, &nf.content) {
            Ok(_) => {
                nf.written = true;
                created += 1;
                log_status!("fix", "Created {}", nf.file);
            }
            Err(e) => {
                log_status!("fix", "Failed to create {}: {}", nf.file, e);
            }
        }
    }

    created
}

/// Apply insertions to file content, returning the modified content.
fn apply_insertions_to_content(
    content: &str,
    insertions: &[Insertion],
    language: &Language,
) -> String {
    let mut result = content.to_string();

    // Categorize insertions by kind
    let mut method_stubs = Vec::new();
    let mut registration_stubs = Vec::new();
    let mut constructor_stubs = Vec::new();
    let mut import_adds = Vec::new();
    let mut trait_uses = Vec::new();
    let mut removals: Vec<(usize, usize)> = Vec::new();

    for insertion in insertions {
        match &insertion.kind {
            InsertionKind::MethodStub => method_stubs.push(&insertion.code),
            InsertionKind::RegistrationStub => registration_stubs.push(&insertion.code),
            InsertionKind::ConstructorWithRegistration => constructor_stubs.push(&insertion.code),
            InsertionKind::ImportAdd => import_adds.push(&insertion.code),
            InsertionKind::TraitUse => trait_uses.push(&insertion.code),
            InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            } => {
                removals.push((*start_line, *end_line));
            }
        }
    }

    // Apply function removals first (before adding imports, to avoid line shifts)
    // Process in reverse order so earlier removals don't invalidate later line numbers
    if !removals.is_empty() {
        removals.sort_by(|a, b| b.0.cmp(&a.0)); // reverse by start_line
        let mut lines: Vec<&str> = result.lines().collect();
        for (start, end) in &removals {
            let start_idx = start.saturating_sub(1); // 1-indexed → 0-indexed
            let end_idx = (*end).min(lines.len());
            if start_idx < lines.len() {
                // Also remove trailing blank line if present
                let remove_end = if end_idx < lines.len() && lines[end_idx].trim().is_empty() {
                    end_idx + 1
                } else {
                    end_idx
                };
                lines.drain(start_idx..remove_end);
            }
        }
        result = lines.join("\n");
        // Preserve trailing newline if original had one
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    // Apply import additions (they go at the top)
    for import_line in &import_adds {
        result = insert_import(&result, import_line, language);
    }

    // Insert trait use statements inside the class body (after opening brace)
    if !trait_uses.is_empty() {
        result = insert_trait_uses(&result, &trait_uses, language);
    }

    // Insert registration stubs into existing __construct
    if !registration_stubs.is_empty() {
        result = insert_into_constructor(&result, &registration_stubs, language);
    }

    // Insert constructor stubs (new __construct with registrations)
    if !constructor_stubs.is_empty() {
        let combined: String = constructor_stubs
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("");
        result = insert_before_closing_brace(&result, &combined, language);
    }

    // Insert method stubs before closing brace
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

/// Insert code into the body of __construct (PHP), new() (Rust), or constructor() (JS).
fn insert_into_constructor(content: &str, stubs: &[&String], language: &Language) -> String {
    let constructor_pattern = match language {
        Language::Php => r"function\s+__construct\s*\([^)]*\)\s*\{",
        Language::Rust => r"fn\s+new\s*\([^)]*\)\s*(?:->[^{]*)?\{",
        Language::JavaScript | Language::TypeScript => r"constructor\s*\([^)]*\)\s*\{",
        Language::Unknown => return content.to_string(),
    };

    let re = match Regex::new(constructor_pattern) {
        Ok(r) => r,
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

/// Insert trait `use` statements inside the class body.
///
/// For PHP: inserts `use TraitName;` after the class opening brace.
/// For Rust: would insert trait impl blocks (not yet implemented).
/// For JS/TS: would insert mixin application (not yet implemented).
fn insert_trait_uses(content: &str, stubs: &[&String], language: &Language) -> String {
    match language {
        Language::Php => {
            // Find the class opening brace: `class Foo ... {`
            let class_re = Regex::new(r"(?:class|trait|interface)\s+\w+[^{]*\{").unwrap();
            if let Some(m) = class_re.find(content) {
                let insert_pos = m.end();
                let mut result = String::with_capacity(content.len() + stubs.len() * 40);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                for stub in stubs {
                    let trimmed = stub.trim_end();
                    result.push_str(trimmed);
                    result.push('\n');
                }
                result.push_str(&content[insert_pos..]);
                result
            } else {
                content.to_string()
            }
        }
        _ => {
            // Other languages: fall back to inserting before closing brace
            let combined: String = stubs
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            insert_before_closing_brace(content, &combined, language)
        }
    }
}

/// Insert code before the last closing brace of a class/struct/impl block.
fn insert_before_closing_brace(content: &str, code: &str, _language: &Language) -> String {
    // Find the last `}` in the file (class/struct closing brace)
    if let Some(last_brace) = content.rfind('}') {
        let mut result = String::with_capacity(content.len() + code.len());
        result.push_str(&content[..last_brace]);
        result.push_str(code);
        result.push_str(&content[last_brace..]);
        result
    } else {
        // No closing brace — append to end
        format!("{}{}", content, code)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_php_signature_with_types() {
        let content = r#"<?php
class MyAbility {
    public function __construct(private Container $container) {}
    public function execute(array $config): array {
        return [];
    }
    public function registerAbility(): void {
        // ...
    }
    protected function helper(): string {
        return '';
    }
}
"#;
        let sigs = extract_php_signatures(content);
        assert_eq!(sigs.len(), 4);

        let execute = sigs.iter().find(|s| s.name == "execute").unwrap();
        assert!(execute.signature.contains("array $config"));
        assert!(execute.signature.contains(": array"));

        let register = sigs.iter().find(|s| s.name == "registerAbility").unwrap();
        assert!(register.signature.contains(": void"));
    }

    #[test]
    fn extract_rust_signature_with_return_type() {
        let content = r#"
pub struct Handler;

impl Handler {
    pub fn new(config: Config) -> Self {
        Self
    }
    pub fn run(&self, input: &str) -> Result<Output> {
        todo!()
    }
    pub(crate) fn validate(&self) -> bool {
        true
    }
}
"#;
        let sigs = extract_rust_signatures(content);
        assert!(sigs.len() >= 2);

        let run = sigs.iter().find(|s| s.name == "run").unwrap();
        assert!(run.signature.contains("&self"));
        assert!(run.signature.contains("Result<Output>"));
    }

    #[test]
    fn generate_php_method_stub() {
        let sig = MethodSignature {
            name: "execute".to_string(),
            signature: "public function execute(array $config): array".to_string(),
            language: Language::Php,
        };
        let stub = generate_method_stub(&sig);
        assert!(stub.contains("public function execute(array $config): array"));
        assert!(stub.contains("throw new \\RuntimeException('Not implemented: execute')"));
    }

    #[test]
    fn generate_rust_method_stub() {
        let sig = MethodSignature {
            name: "run".to_string(),
            signature: "pub fn run(&self) -> Result<()>".to_string(),
            language: Language::Rust,
        };
        let stub = generate_method_stub(&sig);
        assert!(stub.contains("pub fn run(&self) -> Result<()>"));
        assert!(stub.contains("todo!(\"run\")"));
    }

    #[test]
    fn insert_method_before_closing_brace() {
        let content = r#"<?php
class MyClass {
    public function existing() {}
}
"#;
        let stub = "\n    public function newMethod() {\n        // stub\n    }\n";
        let result = insert_before_closing_brace(content, stub, &Language::Php);

        assert!(result.contains("newMethod"));
        assert!(result.contains("existing"));
        // newMethod should appear before the final }
        let new_pos = result.find("newMethod").unwrap();
        let last_brace = result.rfind('}').unwrap();
        assert!(new_pos < last_brace);
    }

    #[test]
    fn insert_registration_into_constructor() {
        let content = r#"<?php
class MyAbility {
    public function __construct() {
        $this->name = 'test';
    }

    public function execute() {}
}
"#;
        let reg = "        add_action('wp_abilities_api_init', [$this, 'abilities_api_init']);"
            .to_string();
        let result = insert_into_constructor(content, &[&reg], &Language::Php);

        assert!(result.contains("add_action('wp_abilities_api_init'"));
        // Registration should be inside __construct
        let construct_pos = result.find("__construct").unwrap();
        let reg_pos = result.find("add_action").unwrap();
        assert!(reg_pos > construct_pos);
    }

    #[test]
    fn constructor_with_registration_when_no_constructor() {
        let content = r#"<?php
class MyAbility {
    public function execute() {}
}
"#;
        let insertions = vec![Insertion {
            kind: InsertionKind::ConstructorWithRegistration,
            code: "\n    public function __construct() {\n        add_action('wp_abilities_api_init', [$this, 'abilities_api_init']);\n    }\n".to_string(),
            description: "Add __construct with registration".to_string(),
        }];

        let result = apply_insertions_to_content(content, &insertions, &Language::Php);

        assert!(result.contains("__construct"));
        assert!(result.contains("add_action"));
        assert!(result.contains("execute")); // existing method preserved
    }

    #[test]
    fn fallback_signature_when_no_conforming_match() {
        let sig = generate_fallback_signature("doSomething", &Language::Php);
        assert_eq!(sig.signature, "public function doSomething()");
        assert_eq!(sig.name, "doSomething");
    }

    #[test]
    fn registration_stub_strips_wp_prefix() {
        let stub = generate_registration_stub("wp_abilities_api_init");
        assert!(stub.contains("'wp_abilities_api_init'"));
        assert!(stub.contains("'abilities_api_init'"));
    }

    #[test]
    fn registration_stub_strips_datamachine_prefix() {
        let stub = generate_registration_stub("datamachine_chat_tools");
        assert!(stub.contains("'datamachine_chat_tools'"));
        assert!(stub.contains("'chat_tools'"));
    }

    #[test]
    fn merged_constructor_with_method_and_registration() {
        // When a file is missing __construct AND a registration,
        // we should get ONE constructor with the registration inside,
        // not two separate insertions.
        use super::super::checks::CheckStatus;
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_merge_test");
        let abilities = dir.join("abilities");
        let _ = std::fs::create_dir_all(&abilities);

        // Conforming file
        std::fs::write(
            abilities.join("GoodAbility.php"),
            r#"<?php
class GoodAbility {
    public function __construct() {
        add_action('wp_abilities_api_init', [$this, 'abilities_api_init']);
    }
    public function execute(array $config): array { return []; }
    public function registerAbility(): void {}
}
"#,
        )
        .unwrap();

        // Outlier: missing __construct AND registration
        std::fs::write(
            abilities.join("BadAbility.php"),
            r#"<?php
class BadAbility {
    public function execute(array $config): array { return []; }
}
"#,
        )
        .unwrap();

        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 2,
                conventions_detected: 1,
                outliers_found: 1,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![ConventionReport {
                name: "Abilities".to_string(),
                glob: "abilities/*".to_string(),
                status: CheckStatus::Drift,
                expected_methods: vec![
                    "__construct".to_string(),
                    "execute".to_string(),
                    "registerAbility".to_string(),
                ],
                expected_registrations: vec!["wp_abilities_api_init".to_string()],
                expected_interfaces: vec![],
                expected_namespace: None,
                expected_imports: vec![],
                conforming: vec!["abilities/GoodAbility.php".to_string()],
                outliers: vec![Outlier {
                    file: "abilities/BadAbility.php".to_string(),
                    deviations: vec![
                        Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: __construct".to_string(),
                            suggestion: "Add __construct()".to_string(),
                        },
                        Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: registerAbility".to_string(),
                            suggestion: "Add registerAbility()".to_string(),
                        },
                        Deviation {
                            kind: DeviationKind::MissingRegistration,
                            description: "Missing registration: wp_abilities_api_init".to_string(),
                            suggestion: "Add wp_abilities_api_init".to_string(),
                        },
                    ],
                }],
                total_files: 2,
                confidence: 0.5,
            }],
            findings: vec![],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);

        assert_eq!(fix_result.fixes.len(), 1);
        let fix = &fix_result.fixes[0];

        // Should have exactly 2 insertions: constructor_with_registration + registerAbility stub
        // NOT 3 (no separate __construct stub)
        assert_eq!(
            fix.insertions.len(),
            2,
            "Expected 2 insertions, got: {:?}",
            fix.insertions
                .iter()
                .map(|i| &i.description)
                .collect::<Vec<_>>()
        );

        let has_constructor_with_reg = fix.insertions.iter().any(|i| {
            matches!(i.kind, InsertionKind::ConstructorWithRegistration)
                && i.code.contains("add_action")
        });
        assert!(
            has_constructor_with_reg,
            "Should have constructor with registration"
        );

        let has_register_ability = fix.insertions.iter().any(|i| {
            matches!(i.kind, InsertionKind::MethodStub) && i.code.contains("registerAbility")
        });
        assert!(has_register_ability, "Should have registerAbility stub");

        // No standalone __construct method stub
        let has_bare_constructor = fix
            .insertions
            .iter()
            .any(|i| matches!(i.kind, InsertionKind::MethodStub) && i.code.contains("__construct"));
        assert!(
            !has_bare_constructor,
            "Should NOT have bare __construct stub"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_fixes_writes_to_disk() {
        let dir = std::env::temp_dir().join("homeboy_fixer_apply_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.php"),
            r#"<?php
class TestClass {
    public function existing() {}
}
"#,
        )
        .unwrap();

        let mut fixes = vec![Fix {
            file: "test.php".to_string(),
            insertions: vec![Insertion {
                kind: InsertionKind::MethodStub,
                code: "\n    public function newMethod(): void {\n        throw new \\RuntimeException('Not implemented: newMethod');\n    }\n".to_string(),
                description: "Add newMethod()".to_string(),
            }],
            applied: false,
        }];

        let applied = apply_fixes(&mut fixes, &dir);
        assert_eq!(applied, 1);
        assert!(fixes[0].applied);

        // Verify file was actually modified
        let content = std::fs::read_to_string(dir.join("test.php")).unwrap();
        assert!(content.contains("newMethod"));
        assert!(content.contains("existing")); // preserved

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_naming_suffix_from_ability_files() {
        let conforming = vec![
            "inc/Abilities/Flow/CreateFlowAbility.php".to_string(),
            "inc/Abilities/Flow/UpdateFlowAbility.php".to_string(),
            "inc/Abilities/Flow/DeleteFlowAbility.php".to_string(),
            "inc/Abilities/Flow/GetFlowsAbility.php".to_string(),
        ];
        let suffix = detect_naming_suffix(&conforming);
        assert_eq!(suffix, Some("Ability".to_string()));
    }

    #[test]
    fn detect_naming_suffix_returns_none_for_diverse_names() {
        let conforming = vec![
            "inc/Core/FileStorage.php".to_string(),
            "inc/Core/AgentMemory.php".to_string(),
            "inc/Core/Workspace.php".to_string(),
        ];
        let suffix = detect_naming_suffix(&conforming);
        // No common suffix — each has different ending
        assert!(suffix.is_none() || suffix == Some("Memory".to_string()).or(None));
    }

    #[test]
    fn extract_class_suffix_pascal_case() {
        assert_eq!(
            extract_class_suffix("CreateFlowAbility"),
            Some("Ability".to_string())
        );
        assert_eq!(
            extract_class_suffix("FlowHelpers"),
            Some("Helpers".to_string())
        );
        assert_eq!(
            extract_class_suffix("BlockSanitizer"),
            Some("Sanitizer".to_string())
        );
    }

    #[test]
    fn suffix_matches_exact() {
        assert!(suffix_matches("CreateFlowAbility", "Ability"));
        assert!(suffix_matches("WebhookTriggerAbility", "Ability"));
        assert!(!suffix_matches("FlowHelpers", "Ability"));
    }

    #[test]
    fn suffix_matches_plural_tolerance() {
        // GitHubAbilities should match convention suffix "Ability"
        assert!(suffix_matches("GitHubAbilities", "Ability"));
        // FetchAbilities should match "Ability"
        assert!(suffix_matches("FetchAbilities", "Ability"));
        // Reverse: singular file, plural suffix
        assert!(suffix_matches("CreateFlowAbility", "Abilities"));
    }

    #[test]
    fn suffix_matches_simple_plural() {
        assert!(suffix_matches("AllTests", "Test"));
        assert!(suffix_matches("SingleTest", "Tests"));
        assert!(suffix_matches("AuthProviders", "Provider"));
    }

    #[test]
    fn suffix_matches_rejects_unrelated() {
        assert!(!suffix_matches("FlowHelpers", "Ability"));
        assert!(!suffix_matches("BlockSanitizer", "Ability"));
        assert!(!suffix_matches("EngineHelpers", "Tool"));
    }

    #[test]
    fn pluralize_y_ending() {
        assert_eq!(pluralize("Ability"), "Abilities");
        assert_eq!(pluralize("Entity"), "Entities");
    }

    #[test]
    fn pluralize_regular() {
        assert_eq!(pluralize("Test"), "Tests");
        assert_eq!(pluralize("Provider"), "Providers");
        assert_eq!(pluralize("Tool"), "Tools");
    }

    #[test]
    fn singularize_ies_ending() {
        assert_eq!(singularize("Abilities"), Some("Ability".to_string()));
        assert_eq!(singularize("Entities"), Some("Entity".to_string()));
    }

    #[test]
    fn singularize_regular_s() {
        assert_eq!(singularize("Tests"), Some("Test".to_string()));
        assert_eq!(singularize("Providers"), Some("Provider".to_string()));
    }

    #[test]
    fn skip_helper_files_in_ability_directory() {
        use super::super::checks::CheckStatus;
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_skip_helper_test");
        let abilities = dir.join("abilities");
        let _ = std::fs::create_dir_all(&abilities);

        // Conforming files with *Ability naming
        for name in &[
            "CreateFlowAbility",
            "UpdateFlowAbility",
            "DeleteFlowAbility",
        ] {
            std::fs::write(
                abilities.join(format!("{}.php", name)),
                format!(
                    r#"<?php
class {} {{
    public function __construct() {{
        add_action('wp_abilities_api_init', [$this, 'registerAbility']);
    }}
    public function execute(array $config): array {{ return []; }}
    public function registerAbility(): void {{}}
}}
"#,
                    name
                ),
            )
            .unwrap();
        }

        // Helper file (outlier)
        std::fs::write(
            abilities.join("FlowHelpers.php"),
            "<?php\nclass FlowHelpers {\n    public function formatFlow() {}\n}\n",
        )
        .unwrap();

        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 4,
                conventions_detected: 1,
                outliers_found: 1,
                alignment_score: Some(0.75),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![ConventionReport {
                name: "Flow".to_string(),
                glob: "abilities/*".to_string(),
                status: CheckStatus::Drift,
                expected_methods: vec![
                    "__construct".to_string(),
                    "execute".to_string(),
                    "registerAbility".to_string(),
                ],
                expected_registrations: vec!["wp_abilities_api_init".to_string()],
                expected_interfaces: vec![],
                expected_namespace: None,
                expected_imports: vec![],
                conforming: vec![
                    "abilities/CreateFlowAbility.php".to_string(),
                    "abilities/UpdateFlowAbility.php".to_string(),
                    "abilities/DeleteFlowAbility.php".to_string(),
                ],
                outliers: vec![Outlier {
                    file: "abilities/FlowHelpers.php".to_string(),
                    deviations: vec![
                        Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: execute".to_string(),
                            suggestion: "Add execute()".to_string(),
                        },
                        Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: registerAbility".to_string(),
                            suggestion: "Add registerAbility()".to_string(),
                        },
                        Deviation {
                            kind: DeviationKind::MissingRegistration,
                            description: "Missing registration: wp_abilities_api_init".to_string(),
                            suggestion: "Add wp_abilities_api_init".to_string(),
                        },
                    ],
                }],
                total_files: 4,
                confidence: 0.75,
            }],
            findings: vec![],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);

        // FlowHelpers should be SKIPPED, not fixed
        assert!(
            fix_result.fixes.is_empty(),
            "Should not generate fixes for FlowHelpers"
        );
        assert_eq!(fix_result.skipped.len(), 1);
        assert!(fix_result.skipped[0].file.contains("FlowHelpers"));
        assert!(fix_result.skipped[0].reason.contains("utility/helper"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_fragmented_conventions() {
        use super::super::checks::CheckStatus;
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_skip_frag_test");
        let _ = std::fs::create_dir_all(&dir);

        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 3,
                conventions_detected: 1,
                outliers_found: 2,
                alignment_score: Some(0.33),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![ConventionReport {
                name: "Jobs".to_string(),
                glob: "jobs/*".to_string(),
                status: CheckStatus::Fragmented,
                expected_methods: vec!["get_job".to_string()],
                expected_registrations: vec![],
                expected_interfaces: vec![],
                expected_namespace: None,
                expected_imports: vec![],
                conforming: vec!["jobs/Jobs.php".to_string()],
                outliers: vec![
                    Outlier {
                        file: "jobs/JobsStatus.php".to_string(),
                        deviations: vec![Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: get_job".to_string(),
                            suggestion: "Add get_job()".to_string(),
                        }],
                    },
                    Outlier {
                        file: "jobs/JobsOps.php".to_string(),
                        deviations: vec![Deviation {
                            kind: DeviationKind::MissingMethod,
                            description: "Missing method: get_job".to_string(),
                            suggestion: "Add get_job()".to_string(),
                        }],
                    },
                ],
                total_files: 3,
                confidence: 0.33,
            }],
            findings: vec![],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);

        // Should be skipped — fragmented convention
        assert!(fix_result.fixes.is_empty());
        assert_eq!(fix_result.skipped.len(), 2);
        assert!(fix_result.skipped[0].reason.contains("confidence too low"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_rust_import_statement() {
        let stmt = generate_import_statement("super::CmdResult", &Language::Rust);
        assert_eq!(stmt, "use super::CmdResult;");
    }

    #[test]
    fn generate_php_import_statement() {
        let stmt = generate_import_statement("DataMachine\\Core\\Base", &Language::Php);
        assert_eq!(stmt, "use DataMachine\\Core\\Base;");
    }

    #[test]
    fn insert_import_after_existing_rust_imports() {
        let content = r#"use serde::Serialize;
use homeboy::project;

pub struct MyOutput {}

pub fn run() {}
"#;
        let result = insert_import(content, "use super::CmdResult;", &Language::Rust);
        assert!(result.contains("use super::CmdResult;"));
        // Should be after the last existing use line
        let cmd_pos = result.find("use super::CmdResult;").unwrap();
        let project_pos = result.find("use homeboy::project;").unwrap();
        assert!(
            cmd_pos > project_pos,
            "New import should be after existing imports"
        );
        // Original content preserved
        assert!(result.contains("pub fn run()"));
    }

    #[test]
    fn insert_import_when_no_existing_imports() {
        let content = r#"// A extension with no imports

pub struct Output {}
"#;
        let result = insert_import(content, "use super::CmdResult;", &Language::Rust);
        assert!(result.contains("use super::CmdResult;"));
        assert!(result.contains("pub struct Output"));
    }

    #[test]
    fn apply_import_add_insertion() {
        let content = r#"use serde::Serialize;

pub struct TestOutput {}
"#;
        let insertions = vec![Insertion {
            kind: InsertionKind::ImportAdd,
            code: "use super::CmdResult;".to_string(),
            description: "Add missing import".to_string(),
        }];

        let result = apply_insertions_to_content(content, &insertions, &Language::Rust);
        assert!(result.contains("use super::CmdResult;"));
        assert!(result.contains("use serde::Serialize;"));
        assert!(result.contains("pub struct TestOutput"));
    }

    #[test]
    fn generate_fixes_handles_missing_import() {
        use super::super::checks::CheckStatus;
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_import_test");
        let commands = dir.join("commands");
        let _ = std::fs::create_dir_all(&commands);

        // Conforming file
        std::fs::write(
            commands.join("good.rs"),
            "use super::CmdResult;\nuse serde::Serialize;\n\npub fn run() {}\n",
        )
        .unwrap();

        // Outlier: missing import
        std::fs::write(commands.join("bad.rs"), "pub fn run() {}\n").unwrap();

        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: dir.to_str().unwrap().to_string(),
            summary: AuditSummary {
                files_scanned: 2,
                conventions_detected: 1,
                outliers_found: 1,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![ConventionReport {
                name: "Commands".to_string(),
                glob: "commands/*".to_string(),
                status: CheckStatus::Drift,
                expected_methods: vec!["run".to_string()],
                expected_registrations: vec![],
                expected_interfaces: vec![],
                expected_namespace: None,
                expected_imports: vec!["super::CmdResult".to_string()],
                conforming: vec!["commands/good.rs".to_string()],
                outliers: vec![Outlier {
                    file: "commands/bad.rs".to_string(),
                    deviations: vec![Deviation {
                        kind: DeviationKind::MissingImport,
                        description: "Missing import: super::CmdResult".to_string(),
                        suggestion: "Add use super::CmdResult;".to_string(),
                    }],
                }],
                total_files: 2,
                confidence: 0.5,
            }],
            findings: vec![],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);
        assert_eq!(fix_result.fixes.len(), 1);
        let fix = &fix_result.fixes[0];
        assert_eq!(fix.file, "commands/bad.rs");
        assert_eq!(fix.insertions.len(), 1);
        assert!(matches!(fix.insertions[0].kind, InsertionKind::ImportAdd));
        assert!(fix.insertions[0].code.contains("use super::CmdResult;"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_expected_test_path_parses_description() {
        let desc =
            "No test file found (expected 'tests/utils/token_test.rs') and no inline tests";
        let parsed = extract_expected_test_path(desc);
        assert_eq!(parsed, Some("tests/utils/token_test.rs".to_string()));
    }

    #[test]
    fn generate_fixes_creates_missing_test_files_from_findings() {
        use super::super::{AuditSummary, CodeAuditResult};

        let dir = std::env::temp_dir().join("homeboy_fixer_missing_test_file");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/utils")).unwrap();

        std::fs::write(dir.join("src/utils/token.rs"), "pub fn tokenize() {}\n").unwrap();

        let audit_result = CodeAuditResult {
            component_id: "homeboy".to_string(),
            source_path: dir.to_string_lossy().to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            findings: vec![super::super::findings::Finding {
                convention: "test_coverage".to_string(),
                severity: super::super::findings::Severity::Info,
                file: "src/utils/token.rs".to_string(),
                description:
                    "No test file found (expected 'tests/utils/token_test.rs') and no inline tests"
                        .to_string(),
                suggestion:
                    "Add tests in 'tests/utils/token_test.rs' or add #[cfg(test)] inline tests"
                        .to_string(),
                kind: DeviationKind::MissingTestFile,
            }],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);
        assert_eq!(fix_result.new_files.len(), 1);
        assert_eq!(fix_result.new_files[0].file, "tests/utils/token_test.rs");
        assert!(fix_result.new_files[0]
            .content
            .contains("test_token_placeholder"));

        let mut new_files = fix_result.new_files.clone();
        let created = apply_new_files(&mut new_files, &dir);
        assert_eq!(created, 1);

        let written = std::fs::read_to_string(dir.join("tests/utils/token_test.rs")).unwrap();
        assert!(written.contains("Auto-generated by `homeboy audit --fix`"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_fixes_dedupes_missing_test_file_creation() {
        use super::super::{AuditSummary, CodeAuditResult};

        let dir = std::env::temp_dir().join("homeboy_fixer_missing_test_file_dedupe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/utils")).unwrap();

        let finding = super::super::findings::Finding {
            convention: "test_coverage".to_string(),
            severity: super::super::findings::Severity::Info,
            file: "src/utils/slugify.rs".to_string(),
            description:
                "No test file found (expected 'tests/utils/slugify_test.rs') and no inline tests"
                    .to_string(),
            suggestion: "Create test file".to_string(),
            kind: DeviationKind::MissingTestFile,
        };

        let audit_result = CodeAuditResult {
            component_id: "homeboy".to_string(),
            source_path: dir.to_string_lossy().to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            findings: vec![finding.clone(), finding],
            directory_conventions: vec![],
            duplicate_groups: vec![],
        };

        let fix_result = generate_fixes(&audit_result, &dir);
        assert_eq!(fix_result.new_files.len(), 1);
        assert_eq!(fix_result.new_files[0].file, "tests/utils/slugify_test.rs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_import_fix_to_disk() {
        let dir = std::env::temp_dir().join("homeboy_fixer_import_apply_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "use serde::Serialize;\n\npub fn run() {}\n",
        )
        .unwrap();

        let mut fixes = vec![Fix {
            file: "test.rs".to_string(),
            insertions: vec![Insertion {
                kind: InsertionKind::ImportAdd,
                code: "use super::CmdResult;".to_string(),
                description: "Add missing import".to_string(),
            }],
            applied: false,
        }];

        let applied = apply_fixes(&mut fixes, &dir);
        assert_eq!(applied, 1);
        assert!(fixes[0].applied);

        let content = std::fs::read_to_string(dir.join("test.rs")).unwrap();
        assert!(content.contains("use super::CmdResult;"));
        assert!(content.contains("use serde::Serialize;"));
        assert!(content.contains("pub fn run()"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_fixes_per_file_combines_same_file() {
        let fixes = vec![
            Fix {
                file: "src/foo.rs".to_string(),
                insertions: vec![Insertion {
                    kind: InsertionKind::FunctionRemoval {
                        start_line: 10,
                        end_line: 20,
                    },
                    code: String::new(),
                    description: "Remove fn_a".to_string(),
                }],
                applied: false,
            },
            Fix {
                file: "src/bar.rs".to_string(),
                insertions: vec![Insertion {
                    kind: InsertionKind::FunctionRemoval {
                        start_line: 5,
                        end_line: 15,
                    },
                    code: String::new(),
                    description: "Remove fn_b from bar".to_string(),
                }],
                applied: false,
            },
            Fix {
                file: "src/foo.rs".to_string(),
                insertions: vec![
                    Insertion {
                        kind: InsertionKind::FunctionRemoval {
                            start_line: 30,
                            end_line: 40,
                        },
                        code: String::new(),
                        description: "Remove fn_c".to_string(),
                    },
                    Insertion {
                        kind: InsertionKind::ImportAdd,
                        code: "use crate::utils::fn_c;".to_string(),
                        description: "Import fn_c".to_string(),
                    },
                ],
                applied: false,
            },
        ];

        let merged = merge_fixes_per_file(fixes);

        // Should have 2 files, not 3
        assert_eq!(merged.len(), 2);

        // foo.rs should have 3 insertions (1 from first + 2 from third)
        let foo = merged.iter().find(|f| f.file == "src/foo.rs").unwrap();
        assert_eq!(foo.insertions.len(), 3);
        assert_eq!(foo.insertions[0].description, "Remove fn_a");
        assert_eq!(foo.insertions[1].description, "Remove fn_c");
        assert_eq!(foo.insertions[2].description, "Import fn_c");

        // bar.rs should have 1 insertion (unchanged)
        let bar = merged.iter().find(|f| f.file == "src/bar.rs").unwrap();
        assert_eq!(bar.insertions.len(), 1);

        // Encounter order preserved: foo first, bar second
        assert_eq!(merged[0].file, "src/foo.rs");
        assert_eq!(merged[1].file, "src/bar.rs");
    }

    #[test]
    fn apply_multiple_removals_same_file() {
        // Simulate the exact bug: 3 function removals in one file
        let content = r#"use std::path::PathBuf;

fn keep_me() -> bool {
    true
}

fn remove_first() -> PathBuf {
    PathBuf::from("/tmp/cache")
}

fn middle_keeper() -> u32 {
    42
}

fn remove_second() -> u64 {
    1234567890
}

fn remove_third() -> bool {
    false
}

fn last_keeper() {
    println!("done");
}
"#;
        let insertions = vec![
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: 7,
                    end_line: 9,
                },
                code: String::new(),
                description: "Remove remove_first".to_string(),
            },
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: 15,
                    end_line: 17,
                },
                code: String::new(),
                description: "Remove remove_second".to_string(),
            },
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: 19,
                    end_line: 21,
                },
                code: String::new(),
                description: "Remove remove_third".to_string(),
            },
            Insertion {
                kind: InsertionKind::ImportAdd,
                code: "use crate::utils::{remove_first, remove_second, remove_third};".to_string(),
                description: "Import removed functions".to_string(),
            },
        ];

        let result = apply_insertions_to_content(content, &insertions, &Language::Rust);

        // Removed functions should be gone
        assert!(
            !result.contains("fn remove_first()"),
            "remove_first should be removed"
        );
        assert!(
            !result.contains("fn remove_second()"),
            "remove_second should be removed"
        );
        assert!(
            !result.contains("fn remove_third()"),
            "remove_third should be removed"
        );

        // Kept functions should survive
        assert!(result.contains("fn keep_me()"), "keep_me should survive");
        assert!(
            result.contains("fn middle_keeper()"),
            "middle_keeper should survive"
        );
        assert!(
            result.contains("fn last_keeper()"),
            "last_keeper should survive"
        );

        // Import should be added
        assert!(result.contains("use crate::utils::{remove_first, remove_second, remove_third};"));
    }

    #[test]
    fn trait_use_inserted_after_class_brace_php() {
        let content = r#"<?php
namespace DataMachine\Abilities;

use DataMachine\Abilities\PermissionHelper;

class FlowAbilities extends BaseAbility {
    public function checkPermission(): bool {
        return PermissionHelper::can_manage();
    }
}
"#;
        let trait_use = "    use HasCheckPermission;".to_string();
        let insertions = vec![Insertion {
            kind: InsertionKind::TraitUse,
            code: trait_use,
            description: "Use shared trait".to_string(),
        }];

        let result = apply_insertions_to_content(content, &insertions, &Language::Php);

        // Trait use should appear inside the class body
        assert!(result
            .contains("class FlowAbilities extends BaseAbility {\n    use HasCheckPermission;\n"));
        // Method should still be there (we only added trait use, no removal)
        assert!(result.contains("checkPermission"));
    }

    #[test]
    fn trait_use_plus_removal_php() {
        let content = r#"<?php
namespace DataMachine\Abilities;

class FlowAbilities {
    public function checkPermission(): bool {
        return true;
    }

    public function execute(): void {
    }
}
"#;
        let insertions = vec![
            Insertion {
                kind: InsertionKind::FunctionRemoval {
                    start_line: 5,
                    end_line: 7,
                },
                code: String::new(),
                description: "Remove duplicate".to_string(),
            },
            Insertion {
                kind: InsertionKind::ImportAdd,
                code: "use DataMachine\\Abilities\\Traits\\HasCheckPermission;".to_string(),
                description: "Import trait".to_string(),
            },
            Insertion {
                kind: InsertionKind::TraitUse,
                code: "    use HasCheckPermission;".to_string(),
                description: "Use trait".to_string(),
            },
        ];

        let result = apply_insertions_to_content(content, &insertions, &Language::Php);

        // Method should be removed
        assert!(
            !result.contains("function checkPermission()"),
            "checkPermission should be removed"
        );
        // Trait use should be present
        assert!(
            result.contains("use HasCheckPermission;"),
            "trait use should be added"
        );
        // Import should be present
        assert!(result.contains("use DataMachine\\Abilities\\Traits\\HasCheckPermission;"));
        // execute method should survive
        assert!(
            result.contains("function execute()"),
            "execute should survive"
        );
    }

    #[test]
    fn new_file_struct() {
        let nf = NewFile {
            file: "inc/Abilities/Traits/HasCheckPermission.php".to_string(),
            content: "<?php\ntrait HasCheckPermission {}".to_string(),
            description: "Create trait".to_string(),
            written: false,
        };
        assert!(!nf.written);
        assert_eq!(nf.file, "inc/Abilities/Traits/HasCheckPermission.php");
    }
}
