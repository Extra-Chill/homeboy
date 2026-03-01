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
use super::CodeAuditResult;

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
}

/// A file that was skipped by the fixer with a reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkippedFile {
    /// Relative file path.
    pub file: String,
    /// Why it was skipped.
    pub reason: String,
}

/// Result of running the fixer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FixResult {
    pub fixes: Vec<Fix>,
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
    let fn_re = Regex::new(
        r"(?m)^\s*((?:export\s+)?(?:async\s+)?function\s+(\w+)\s*\([^)]*\))",
    )
    .unwrap();
    // Class methods
    let method_re = Regex::new(
        r"(?m)^\s+((?:async\s+)?(\w+)\s*\([^)]*\))\s*\{",
    )
    .unwrap();

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
            let name = import_path.rsplit("::").next()
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
        if first_code > 0 { first_code - 1 } else { 0 }
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

    format!("        add_action('{}', [$this, '{}']);", hook_name, callback)
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

                        if method_name == "__construct" || method_name == "new" || method_name == "constructor" {
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
                            description: format!(
                                "Add {} registration in __construct()",
                                hook_name
                            ),
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

    let total_insertions: usize = fixes.iter().map(|f| f.insertions.len()).sum();
    let files_modified = fixes.len();

    FixResult {
        fixes,
        skipped,
        total_insertions,
        files_modified,
    }
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
    if word.ends_with('y') && !word.ends_with("ey") && !word.ends_with("ay") && !word.ends_with("oy") {
        // Ability → Abilities, Entity → Entities
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with('s') || word.ends_with('x') || word.ends_with("ch") || word.ends_with("sh") {
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
    } else if word.ends_with("ses") || word.ends_with("xes") || word.ends_with("ches") || word.ends_with("shes") {
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
                    log_status!("fix", "Applied {} fix(es) to {}", fix.insertions.len(), fix.file);
                }
                Err(e) => {
                    log_status!("fix", "Failed to write {}: {}", fix.file, e);
                }
            }
        }
    }

    applied_count
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

    for insertion in insertions {
        match insertion.kind {
            InsertionKind::MethodStub => method_stubs.push(&insertion.code),
            InsertionKind::RegistrationStub => registration_stubs.push(&insertion.code),
            InsertionKind::ConstructorWithRegistration => constructor_stubs.push(&insertion.code),
            InsertionKind::ImportAdd => import_adds.push(&insertion.code),
        }
    }

    // Apply import additions first (they go at the top)
    for import_line in &import_adds {
        result = insert_import(&result, import_line, language);
    }

    // Insert registration stubs into existing __construct
    if !registration_stubs.is_empty() {
        result = insert_into_constructor(&result, &registration_stubs, language);
    }

    // Insert constructor stubs (new __construct with registrations)
    if !constructor_stubs.is_empty() {
        let combined: String = constructor_stubs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("");
        result = insert_before_closing_brace(&result, &combined, language);
    }

    // Insert method stubs before closing brace
    if !method_stubs.is_empty() {
        let combined: String = method_stubs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("");
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
        let reg = "        add_action('wp_abilities_api_init', [$this, 'abilities_api_init']);".to_string();
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
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::checks::CheckStatus;
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
        ).unwrap();

        // Outlier: missing __construct AND registration
        std::fs::write(
            abilities.join("BadAbility.php"),
            r#"<?php
class BadAbility {
    public function execute(array $config): array { return []; }
}
"#,
        ).unwrap();

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
        };

        let fix_result = generate_fixes(&audit_result, &dir);

        assert_eq!(fix_result.fixes.len(), 1);
        let fix = &fix_result.fixes[0];

        // Should have exactly 2 insertions: constructor_with_registration + registerAbility stub
        // NOT 3 (no separate __construct stub)
        assert_eq!(fix.insertions.len(), 2, "Expected 2 insertions, got: {:?}",
            fix.insertions.iter().map(|i| &i.description).collect::<Vec<_>>());

        let has_constructor_with_reg = fix.insertions.iter().any(|i|
            matches!(i.kind, InsertionKind::ConstructorWithRegistration)
            && i.code.contains("add_action")
        );
        assert!(has_constructor_with_reg, "Should have constructor with registration");

        let has_register_ability = fix.insertions.iter().any(|i|
            matches!(i.kind, InsertionKind::MethodStub)
            && i.code.contains("registerAbility")
        );
        assert!(has_register_ability, "Should have registerAbility stub");

        // No standalone __construct method stub
        let has_bare_constructor = fix.insertions.iter().any(|i|
            matches!(i.kind, InsertionKind::MethodStub)
            && i.code.contains("__construct")
        );
        assert!(!has_bare_constructor, "Should NOT have bare __construct stub");

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
        ).unwrap();

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
        assert_eq!(extract_class_suffix("CreateFlowAbility"), Some("Ability".to_string()));
        assert_eq!(extract_class_suffix("FlowHelpers"), Some("Helpers".to_string()));
        assert_eq!(extract_class_suffix("BlockSanitizer"), Some("Sanitizer".to_string()));
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
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::checks::CheckStatus;
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_skip_helper_test");
        let abilities = dir.join("abilities");
        let _ = std::fs::create_dir_all(&abilities);

        // Conforming files with *Ability naming
        for name in &["CreateFlowAbility", "UpdateFlowAbility", "DeleteFlowAbility"] {
            std::fs::write(
                abilities.join(format!("{}.php", name)),
                format!(r#"<?php
class {} {{
    public function __construct() {{
        add_action('wp_abilities_api_init', [$this, 'registerAbility']);
    }}
    public function execute(array $config): array {{ return []; }}
    public function registerAbility(): void {{}}
}}
"#, name),
            ).unwrap();
        }

        // Helper file (outlier)
        std::fs::write(
            abilities.join("FlowHelpers.php"),
            "<?php\nclass FlowHelpers {\n    public function formatFlow() {}\n}\n",
        ).unwrap();

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
        };

        let fix_result = generate_fixes(&audit_result, &dir);

        // FlowHelpers should be SKIPPED, not fixed
        assert!(fix_result.fixes.is_empty(), "Should not generate fixes for FlowHelpers");
        assert_eq!(fix_result.skipped.len(), 1);
        assert!(fix_result.skipped[0].file.contains("FlowHelpers"));
        assert!(fix_result.skipped[0].reason.contains("utility/helper"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_fragmented_conventions() {
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::checks::CheckStatus;
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
        assert!(cmd_pos > project_pos, "New import should be after existing imports");
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
        use super::super::conventions::{Deviation, DeviationKind, Outlier};
        use super::super::checks::CheckStatus;
        use super::super::{AuditSummary, CodeAuditResult, ConventionReport};

        let dir = std::env::temp_dir().join("homeboy_fixer_import_test");
        let commands = dir.join("commands");
        let _ = std::fs::create_dir_all(&commands);

        // Conforming file
        std::fs::write(
            commands.join("good.rs"),
            "use super::CmdResult;\nuse serde::Serialize;\n\npub fn run() {}\n",
        ).unwrap();

        // Outlier: missing import
        std::fs::write(
            commands.join("bad.rs"),
            "pub fn run() {}\n",
        ).unwrap();

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
    fn apply_import_fix_to_disk() {
        let dir = std::env::temp_dir().join("homeboy_fixer_import_apply_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("test.rs"),
            "use serde::Serialize;\n\npub fn run() {}\n",
        ).unwrap();

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
}
