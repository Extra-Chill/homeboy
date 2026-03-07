//! Test cross-reference — static analysis of test/production symbol alignment.
//!
//! Complements `test_drift` (git-diff-based) with static cross-reference:
//! scans test files and production files to find mismatches that aren't
//! visible from git history alone.
//!
//! Language-agnostic core. Extension scripts provide language-specific
//! extraction (hook names, mock expectations, method calls) via the
//! `scripts.crossref` protocol.
//!
//! Detected mismatch types:
//! - Hook mismatch: test registers for a hook that production never fires
//! - Mock mismatch: test mocks a method that production doesn't call
//! - Arg count mismatch: test registers with wrong argument count

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::extension::{
    self, CrossRefExtraction, ExtensionManifest, HookReference, MethodCall, MockExpectation,
};
use crate::refactor::TransformRule;

// ============================================================================
// Models
// ============================================================================

/// A mismatch found between test expectations and production reality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRefMismatch {
    /// Type of mismatch.
    pub mismatch_type: MismatchType,
    /// The test file where the stale reference exists.
    pub test_file: String,
    /// Line number in the test file.
    pub test_line: usize,
    /// The stale symbol used in the test.
    pub test_symbol: String,
    /// The correct symbol from production (if a match was found).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_symbol: Option<String>,
    /// Production file where the correct symbol lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_file: Option<String>,
    /// Human-readable description.
    pub description: String,
    /// Whether this mismatch can be auto-fixed.
    pub auto_fixable: bool,
}

/// Type of cross-reference mismatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MismatchType {
    /// Test hooks a name that production never fires.
    HookNotFound,
    /// Test hooks a name that's similar to a production hook (likely renamed).
    HookRenamed,
    /// Test registers with wrong argument count for the hook.
    HookArgCount,
    /// Test mocks a method that production never calls on that class.
    MockMethodNotFound,
    /// Test mocks a method that's similar to a production method (likely renamed).
    MockMethodRenamed,
}

/// Full cross-reference report.
#[derive(Debug, Clone, Serialize)]
pub struct CrossRefReport {
    /// Component name.
    pub component: String,
    /// All mismatches found.
    pub mismatches: Vec<CrossRefMismatch>,
    /// Summary counts.
    pub hook_mismatches: usize,
    pub mock_mismatches: usize,
    pub total_auto_fixable: usize,
    /// Stats.
    pub test_files_scanned: usize,
    pub production_files_scanned: usize,
}

// ============================================================================
// Cross-reference engine
// ============================================================================

/// Options for cross-reference analysis.
pub struct CrossRefOptions<'a> {
    /// Component root directory.
    pub root: &'a Path,
    /// Glob patterns for production source files.
    pub source_patterns: Vec<String>,
    /// Glob patterns for test files.
    pub test_patterns: Vec<String>,
}

impl<'a> CrossRefOptions<'a> {
    /// Create options with PHP defaults.
    pub fn php(root: &'a Path) -> Self {
        Self {
            root,
            source_patterns: vec![
                "src/**/*.php".into(),
                "inc/**/*.php".into(),
                "lib/**/*.php".into(),
            ],
            test_patterns: vec!["tests/**/*.php".into()],
        }
    }

    /// Create options with Rust defaults.
    pub fn rust(root: &'a Path) -> Self {
        Self {
            root,
            source_patterns: vec!["src/**/*.rs".into()],
            test_patterns: vec!["tests/**/*.rs".into()],
        }
    }
}

/// Run cross-reference analysis between test and production files.
///
/// Uses the extension's crossref script to extract hook registrations,
/// mock expectations, etc. from both test and production files, then
/// cross-references to find mismatches.
pub fn analyze(
    component: &str,
    opts: &CrossRefOptions,
    ext: Option<&ExtensionManifest>,
) -> CrossRefReport {
    let test_files = collect_files(opts.root, true);
    let prod_files = collect_files(opts.root, false);

    // Extract cross-reference data from all files
    let test_extractions = extract_all(&test_files, opts.root, ext);
    let prod_extractions = extract_all(&prod_files, opts.root, ext);

    // Build production indexes
    let prod_hooks = build_hook_index(&prod_extractions, "definition");
    let prod_methods = build_method_index(&prod_extractions);

    // Find mismatches
    let mut mismatches = Vec::new();

    // Check hook registrations in tests against production definitions
    for extraction in &test_extractions {
        for reg in &extraction.hook_registrations {
            if let Some(mismatch) = check_hook_mismatch(reg, &prod_hooks) {
                mismatches.push(mismatch);
            }
        }

        for mock in &extraction.mock_expectations {
            if let Some(mismatch) = check_mock_mismatch(mock, &prod_methods) {
                mismatches.push(mismatch);
            }
        }
    }

    let hook_mismatches = mismatches
        .iter()
        .filter(|m| matches!(m.mismatch_type, MismatchType::HookNotFound | MismatchType::HookRenamed | MismatchType::HookArgCount))
        .count();
    let mock_mismatches = mismatches
        .iter()
        .filter(|m| matches!(m.mismatch_type, MismatchType::MockMethodNotFound | MismatchType::MockMethodRenamed))
        .count();
    let total_auto_fixable = mismatches.iter().filter(|m| m.auto_fixable).count();

    CrossRefReport {
        component: component.to_string(),
        mismatches,
        hook_mismatches,
        mock_mismatches,
        total_auto_fixable,
        test_files_scanned: test_files.len(),
        production_files_scanned: prod_files.len(),
    }
}

/// Generate transform rules from cross-reference mismatches.
pub fn generate_transform_rules(report: &CrossRefReport) -> Vec<TransformRule> {
    let mut rules = Vec::new();

    for mismatch in &report.mismatches {
        if !mismatch.auto_fixable {
            continue;
        }

        let production_symbol = match &mismatch.production_symbol {
            Some(s) => s,
            None => continue,
        };

        let (id, find, replace, description) = match mismatch.mismatch_type {
            MismatchType::HookRenamed => {
                let id = format!("hook_rename_{}", mismatch.test_symbol);
                // Use word-boundary-aware match within string literals
                let find = regex::escape(&mismatch.test_symbol);
                let replace = production_symbol.clone();
                let desc = format!(
                    "Hook renamed: '{}' → '{}' ({})",
                    mismatch.test_symbol,
                    production_symbol,
                    mismatch.production_file.as_deref().unwrap_or("unknown"),
                );
                (id, find, replace, desc)
            }
            MismatchType::MockMethodRenamed => {
                let id = format!("mock_rename_{}", mismatch.test_symbol);
                let find = format!(r"\b{}\b", regex::escape(&mismatch.test_symbol));
                let replace = production_symbol.clone();
                let desc = format!(
                    "Mock method renamed: {} → {} ({})",
                    mismatch.test_symbol,
                    production_symbol,
                    mismatch.production_file.as_deref().unwrap_or("unknown"),
                );
                (id, find, replace, desc)
            }
            _ => continue,
        };

        rules.push(TransformRule {
            id,
            description,
            find,
            replace,
            files: "tests/**/*".to_string(),
            context: "line".to_string(),
        });
    }

    // Deduplicate rules by id
    let mut seen = std::collections::HashSet::new();
    rules.retain(|r| seen.insert(r.id.clone()));

    rules
}

// ============================================================================
// Extraction
// ============================================================================

/// Extract cross-reference data from all files using the extension script.
fn extract_all(
    files: &[PathBuf],
    root: &Path,
    ext: Option<&ExtensionManifest>,
) -> Vec<CrossRefExtraction> {
    files
        .iter()
        .filter_map(|file| {
            let content = std::fs::read_to_string(file).ok()?;
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .to_string();

            extract_from_file(&relative, &content, ext)
        })
        .collect()
}

/// Extract cross-reference data from a single file.
///
/// Tries extension script first, falls back to regex-based extraction.
fn extract_from_file(
    file: &str,
    content: &str,
    ext: Option<&ExtensionManifest>,
) -> Option<CrossRefExtraction> {
    // Try extension script
    if let Some(ext) = ext {
        let command = serde_json::json!({
            "command": "extract_crossref",
            "file": file,
            "content": content,
        });

        if let Some(result) = extension::run_crossref_script(ext, &command) {
            if let Ok(extraction) = serde_json::from_value::<CrossRefExtraction>(result) {
                return Some(extraction);
            }
        }
    }

    // Fallback: regex-based extraction (language-agnostic patterns)
    Some(fallback_extract(file, content))
}

/// Regex-based fallback extraction for common patterns.
///
/// Catches the most common hook/mock patterns without extension scripts.
/// Less precise than language-specific extraction but works everywhere.
fn fallback_extract(file: &str, content: &str) -> CrossRefExtraction {
    let mut extraction = CrossRefExtraction::default();
    let is_test = file.contains("/tests/") || file.contains("Test.");

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip comments
        if trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('*')
            || trimmed.starts_with("/*")
        {
            continue;
        }

        if is_test {
            // Look for hook registrations in test files
            extract_hook_registrations(trimmed, file, i + 1, &mut extraction.hook_registrations);
            // Look for mock expectations in test files
            extract_mock_expectations(trimmed, file, i + 1, &mut extraction.mock_expectations);
        } else {
            // Look for hook definitions in production files
            extract_hook_definitions(trimmed, file, i + 1, &mut extraction.hook_definitions);
            // Look for method calls in production files
            extract_method_calls(trimmed, file, i + 1, &mut extraction.method_calls);
        }
    }

    extraction
}

/// Extract hook registration patterns (test files).
/// Patterns: add_filter('name', ..., N), add_action('name', ...), subscribe('name', ...)
fn extract_hook_registrations(
    line: &str,
    file: &str,
    line_num: usize,
    results: &mut Vec<HookReference>,
) {
    // PHP: add_filter('hook_name', ..., priority, arg_count)
    // PHP: add_action('hook_name', ...)
    let registration_patterns = [
        ("add_filter", true),
        ("add_action", false),
        ("remove_filter", false),
        ("remove_action", false),
    ];

    for (func, has_args) in &registration_patterns {
        if let Some(pos) = line.find(func) {
            if let Some(name) = extract_string_arg(line, pos + func.len()) {
                let args_count = if *has_args {
                    extract_args_count(line, pos + func.len())
                } else {
                    None
                };

                results.push(HookReference {
                    name,
                    file: file.to_string(),
                    line: line_num,
                    args_count,
                    kind: "registration".to_string(),
                });
            }
        }
    }
}

/// Extract hook definition patterns (production files).
/// Patterns: apply_filters('name', ...), do_action('name', ...)
fn extract_hook_definitions(
    line: &str,
    file: &str,
    line_num: usize,
    results: &mut Vec<HookReference>,
) {
    let definition_patterns = ["apply_filters", "do_action"];

    for func in &definition_patterns {
        if let Some(pos) = line.find(func) {
            if let Some(name) = extract_string_arg(line, pos + func.len()) {
                let args_count = count_filter_args(line, pos + func.len());

                results.push(HookReference {
                    name,
                    file: file.to_string(),
                    line: line_num,
                    args_count: Some(args_count),
                    kind: "definition".to_string(),
                });
            }
        }
    }
}

// Pre-compiled regexes for extraction (compiled once, reused across all files).
static MOCK_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"->method\(\s*['"](\w+)['"]\s*\)"#).unwrap());
static INSTANCE_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\$(\w+)->(\w+)\s*\("#).unwrap());
static STATIC_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(\w+)::(\w+)\s*\("#).unwrap());

/// Extract mock method expectations from test files.
/// Patterns: ->method('methodName'), ->expects(...)->method('methodName')
fn extract_mock_expectations(
    line: &str,
    file: &str,
    line_num: usize,
    results: &mut Vec<MockExpectation>,
) {
    for cap in MOCK_METHOD_RE.captures_iter(line) {
        // Try to find the class from createMock() or getMockBuilder() in nearby context
        // For now, extract just the method name — class comes from file-level analysis
        let method_name = cap[1].to_string();

        results.push(MockExpectation {
            class: String::new(), // Populated by extension script or context analysis
            method: method_name,
            file: file.to_string(),
            line: line_num,
        });
    }
}

/// Extract method calls from production code.
/// Patterns: $obj->method(), ClassName::method()
fn extract_method_calls(
    line: &str,
    file: &str,
    line_num: usize,
    results: &mut Vec<MethodCall>,
) {
    // Pattern: $variable->methodName(
    for cap in INSTANCE_CALL_RE.captures_iter(line) {
        results.push(MethodCall {
            class: format!("${}", &cap[1]),
            method: cap[2].to_string(),
            file: file.to_string(),
            line: line_num,
        });
    }

    // Pattern: ClassName::methodName(
    for cap in STATIC_CALL_RE.captures_iter(line) {
        results.push(MethodCall {
            class: cap[1].to_string(),
            method: cap[2].to_string(),
            file: file.to_string(),
            line: line_num,
        });
    }
}

// ============================================================================
// String extraction helpers
// ============================================================================

/// Extract a string argument from a function call.
/// Given position after function name, finds the first quoted string in parens.
fn extract_string_arg(line: &str, start: usize) -> Option<String> {
    let rest = line.get(start..)?;

    // Find opening paren
    let paren_pos = rest.find('(')?;
    let after_paren = rest.get(paren_pos + 1..)?;

    // Find quoted string
    let quote_chars = ['\'', '"'];
    for &q in &quote_chars {
        if let Some(start_q) = after_paren.find(q) {
            let after_start = after_paren.get(start_q + 1..)?;
            if let Some(end_q) = after_start.find(q) {
                return Some(after_start[..end_q].to_string());
            }
        }
    }

    None
}

/// Extract the argument count from a filter registration.
/// add_filter('name', callback, priority, arg_count) — arg_count is the 4th arg.
fn extract_args_count(line: &str, start: usize) -> Option<usize> {
    let rest = line.get(start..)?;
    let paren_pos = rest.find('(')?;
    let paren_content = rest.get(paren_pos + 1..)?;

    // Count commas to find 4th argument
    let mut depth = 0;
    let mut comma_count = 0;
    let mut arg_start = 0;

    for (i, c) in paren_content.chars().enumerate() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => {
                comma_count += 1;
                if comma_count == 3 {
                    arg_start = i + 1;
                }
            }
            _ => {}
        }
    }

    if comma_count >= 3 {
        let arg = paren_content.get(arg_start..)?.trim();
        let arg = arg.trim_end_matches(')').trim();
        arg.parse().ok()
    } else {
        None
    }
}

/// Count the number of arguments passed to apply_filters/do_action.
/// apply_filters('name', $arg1, $arg2) → 2 (excluding the hook name).
fn count_filter_args(line: &str, start: usize) -> usize {
    let rest = match line.get(start..) {
        Some(r) => r,
        None => return 0,
    };
    let paren_pos = match rest.find('(') {
        Some(p) => p,
        None => return 0,
    };
    let paren_content = match rest.get(paren_pos + 1..) {
        Some(c) => c,
        None => return 0,
    };

    let mut depth = 0;
    let mut comma_count = 0;

    for c in paren_content.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => comma_count += 1,
            _ => {}
        }
    }

    // Number of args = commas + 1 (for the hook name). But we want args excluding the name.
    // So: commas = separators between all args, first arg is hook name.
    comma_count // This equals the number of additional args after the hook name.
}

// ============================================================================
// Index building
// ============================================================================

/// Index of production hook definitions by name.
type HookIndex = HashMap<String, Vec<HookReference>>;

fn build_hook_index(extractions: &[CrossRefExtraction], kind: &str) -> HookIndex {
    let mut index: HookIndex = HashMap::new();

    for extraction in extractions {
        let hooks = if kind == "definition" {
            &extraction.hook_definitions
        } else {
            &extraction.hook_registrations
        };

        for hook in hooks {
            index
                .entry(hook.name.clone())
                .or_default()
                .push(hook.clone());
        }
    }

    index
}

/// Index of production method calls by class::method key.
type MethodIndex = HashMap<String, Vec<MethodCall>>;

fn build_method_index(extractions: &[CrossRefExtraction]) -> MethodIndex {
    let mut index: MethodIndex = HashMap::new();

    for extraction in extractions {
        for call in &extraction.method_calls {
            // Index by method name alone for fuzzy matching
            index
                .entry(call.method.clone())
                .or_default()
                .push(call.clone());
        }
    }

    index
}

// ============================================================================
// Mismatch detection
// ============================================================================

/// Check if a test hook registration has a matching production definition.
fn check_hook_mismatch(
    registration: &HookReference,
    prod_hooks: &HookIndex,
) -> Option<CrossRefMismatch> {
    // Exact match — hook exists in production
    if let Some(defs) = prod_hooks.get(&registration.name) {
        // Check arg count mismatch
        if let Some(test_args) = registration.args_count {
            for def in defs {
                if let Some(prod_args) = def.args_count {
                    if test_args != prod_args {
                        return Some(CrossRefMismatch {
                            mismatch_type: MismatchType::HookArgCount,
                            test_file: registration.file.clone(),
                            test_line: registration.line,
                            test_symbol: registration.name.clone(),
                            production_symbol: Some(format!(
                                "{} (expects {} args, test provides {})",
                                registration.name, prod_args, test_args
                            )),
                            production_file: Some(def.file.clone()),
                            description: format!(
                                "Hook '{}' arg count mismatch: test={}, production={}",
                                registration.name, test_args, prod_args
                            ),
                            auto_fixable: false, // Arg count fix needs context
                        });
                    }
                }
            }
        }
        return None; // Exact match found, no mismatch
    }

    // No exact match — look for similar names (possible rename)
    if let Some((best_name, best_file)) = find_similar_hook(&registration.name, prod_hooks) {
        return Some(CrossRefMismatch {
            mismatch_type: MismatchType::HookRenamed,
            test_file: registration.file.clone(),
            test_line: registration.line,
            test_symbol: registration.name.clone(),
            production_symbol: Some(best_name.clone()),
            production_file: Some(best_file),
            description: format!(
                "Hook '{}' not found in production, similar hook '{}' exists (likely renamed)",
                registration.name, best_name
            ),
            auto_fixable: true,
        });
    }

    // Prefix-stripping: if a test hook uses a prefix like "pre_" for interception,
    // try matching the base name (without prefix) against production hooks.
    // Common in frameworks where interceptor hooks wrap the real hook.
    // e.g., test registers "pre_datamachine_ai_request" → base "datamachine_ai_request"
    //        → production has "chubes_ai_request" → suggest the base hook directly.
    let interceptor_prefixes = ["pre_", "before_", "after_", "on_"];
    for prefix in &interceptor_prefixes {
        if let Some(base_name) = registration.name.strip_prefix(prefix) {
            // Check if base name exactly matches a production hook
            if prod_hooks.contains_key(base_name) {
                return Some(CrossRefMismatch {
                    mismatch_type: MismatchType::HookRenamed,
                    test_file: registration.file.clone(),
                    test_line: registration.line,
                    test_symbol: registration.name.clone(),
                    production_symbol: Some(base_name.to_string()),
                    production_file: prod_hooks
                        .get(base_name)
                        .and_then(|refs| refs.first().map(|r| r.file.clone())),
                    description: format!(
                        "Hook '{}' uses interceptor prefix '{}', production has '{}'",
                        registration.name, prefix, base_name
                    ),
                    auto_fixable: true,
                });
            }
            // Check if base name has a similar match in production
            if let Some((best_name, best_file)) = find_similar_hook(base_name, prod_hooks) {
                return Some(CrossRefMismatch {
                    mismatch_type: MismatchType::HookRenamed,
                    test_file: registration.file.clone(),
                    test_line: registration.line,
                    test_symbol: registration.name.clone(),
                    production_symbol: Some(best_name.clone()),
                    production_file: Some(best_file),
                    description: format!(
                        "Hook '{}' (base: '{}') similar to production hook '{}'",
                        registration.name, base_name, best_name
                    ),
                    auto_fixable: true,
                });
            }
        }
    }

    // No match and no similar name — genuinely missing
    Some(CrossRefMismatch {
        mismatch_type: MismatchType::HookNotFound,
        test_file: registration.file.clone(),
        test_line: registration.line,
        test_symbol: registration.name.clone(),
        production_symbol: None,
        production_file: None,
        description: format!(
            "Hook '{}' registered in test but not found in production",
            registration.name
        ),
        auto_fixable: false,
    })
}

/// Check if a mock method expectation matches a production method call.
fn check_mock_mismatch(
    mock: &MockExpectation,
    prod_methods: &MethodIndex,
) -> Option<CrossRefMismatch> {
    // Exact match — method exists in production
    if prod_methods.contains_key(&mock.method) {
        return None;
    }

    // Look for similar method names (possible rename)
    if let Some((best_method, best_file)) = find_similar_method(&mock.method, prod_methods) {
        return Some(CrossRefMismatch {
            mismatch_type: MismatchType::MockMethodRenamed,
            test_file: mock.file.clone(),
            test_line: mock.line,
            test_symbol: mock.method.clone(),
            production_symbol: Some(best_method.clone()),
            production_file: Some(best_file),
            description: format!(
                "Mock expects '{}' but production uses '{}' (likely renamed)",
                mock.method, best_method
            ),
            auto_fixable: true,
        });
    }

    // Not found at all
    Some(CrossRefMismatch {
        mismatch_type: MismatchType::MockMethodNotFound,
        test_file: mock.file.clone(),
        test_line: mock.line,
        test_symbol: mock.method.clone(),
        production_symbol: None,
        production_file: None,
        description: format!(
            "Mock expects method '{}' which is not called anywhere in production",
            mock.method
        ),
        auto_fixable: false,
    })
}

// ============================================================================
// Similarity matching
// ============================================================================

/// Find the most similar hook name in the production index.
///
/// Uses a composite score: LCS ratio + suffix bonus. For hook names that follow
/// `{namespace}_{feature}` patterns (e.g., `datamachine_ai_request`), shared
/// suffixes indicate functional similarity and are weighted more heavily than
/// prefix matches alone. This prevents namespace-matching false positives like
/// `pre_datamachine_directives` beating `pre_chubes_ai_request` when the test
/// hook is `pre_datamachine_ai_request`.
fn find_similar_hook(name: &str, index: &HookIndex) -> Option<(String, String)> {
    let mut best: Option<(String, String, f64)> = None;

    for (prod_name, refs) in index {
        let score = hook_similarity_score(name, prod_name);
        if score > 0.75 {
            let file = refs.first().map(|r| r.file.clone()).unwrap_or_default();
            if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                best = Some((prod_name.clone(), file, score));
            }
        }
    }

    best.map(|(name, file, _)| (name, file))
}

/// Compute hook-specific similarity score with suffix weighting.
///
/// For underscore-delimited hook names, splits into segments and computes:
/// - Base LCS ratio (0.0-1.0)
/// - Suffix bonus: shared trailing segments add weight because they indicate
///   functional similarity (e.g., `_ai_request` suffix means "AI request hook")
///
/// This ensures `pre_datamachine_ai_request` matches `pre_chubes_ai_request`
/// (same function, different namespace) over `pre_datamachine_directives`
/// (same namespace, different function).
fn hook_similarity_score(a: &str, b: &str) -> f64 {
    let base = similarity_score(a, b);

    // Split on underscores and compare trailing segments.
    let a_parts: Vec<&str> = a.split('_').collect();
    let b_parts: Vec<&str> = b.split('_').collect();

    // Count shared trailing segments.
    let mut shared_suffix = 0;
    let min_len = a_parts.len().min(b_parts.len());
    for i in 0..min_len {
        let a_idx = a_parts.len() - 1 - i;
        let b_idx = b_parts.len() - 1 - i;
        if a_parts[a_idx] == b_parts[b_idx] {
            shared_suffix += 1;
        } else {
            break;
        }
    }

    if min_len > 1 {
        if shared_suffix > 0 {
            // Suffix bonus: shared trailing segments indicate functional similarity.
            // Scale bonus by both the count and significance of shared segments.
            let suffix_ratio = shared_suffix as f64 / min_len as f64;
            let bonus = suffix_ratio * 0.3;
            (base + bonus).min(1.0)
        } else {
            // Suffix penalty: names that share ZERO trailing segments are likely
            // different features in the same namespace. Apply a small penalty
            // to prefer functional matches over namespace matches.
            (base - 0.05).max(0.0)
        }
    } else {
        base
    }
}

/// Find the most similar method name in the production index.
fn find_similar_method(name: &str, index: &MethodIndex) -> Option<(String, String)> {
    let mut best: Option<(String, String, f64)> = None;

    for (prod_method, calls) in index {
        let score = similarity_score(name, prod_method);
        if score > 0.5 {
            let file = calls.first().map(|c| c.file.clone()).unwrap_or_default();
            if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                best = Some((prod_method.clone(), file, score));
            }
        }
    }

    best.map(|(name, file, _)| (name, file))
}

/// Compute similarity between two strings.
/// Uses longest common subsequence ratio.
fn similarity_score(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let lcs_len = longest_common_subsequence_len(a, b);
    let max_len = a.len().max(b.len()) as f64;
    lcs_len as f64 / max_len
}

/// Compute length of the longest common subsequence.
fn longest_common_subsequence_len(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a_chars[i - 1] == b_chars[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = prev[j].max(curr[j - 1]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|x| *x = 0);
    }

    prev.into_iter().max().unwrap_or(0)
}

// ============================================================================
// File collection
// ============================================================================

/// Collect test or production files from the component root.
fn collect_files(root: &Path, test_files: bool) -> Vec<PathBuf> {
    let target = if test_files {
        root.join("tests")
    } else {
        // Walk inc/, src/, lib/ for production files
        root.to_path_buf()
    };

    if !target.exists() {
        return Vec::new();
    }

    let mut files = Vec::new();
    collect_recursive(&target, root, &mut files, test_files);
    files
}

fn collect_recursive(dir: &Path, root: &Path, files: &mut Vec<PathBuf>, test_files: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Skip VCS, dependencies, and build dirs
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | "vendor" | "build" | "dist" | "target" | "cache"
            ) {
                continue;
            }

            // For production files, skip the tests directory
            if !test_files && name == "tests" {
                continue;
            }

            collect_recursive(&path, root, files, test_files);
        } else if path.is_file() {
            // Only include source files
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "php" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx") {
                files.push(path);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_string_arg_single_quotes() {
        let line = "add_filter( 'my_hook', 'callback', 10, 3 )";
        let result = extract_string_arg(line, 10); // after "add_filter"
        assert_eq!(result, Some("my_hook".to_string()));
    }

    #[test]
    fn extract_string_arg_double_quotes() {
        let line = r#"add_filter( "my_hook", $callback )"#;
        let result = extract_string_arg(line, 10);
        assert_eq!(result, Some("my_hook".to_string()));
    }

    #[test]
    fn extract_args_count_present() {
        let line = "add_filter( 'my_hook', 'callback', 10, 7 )";
        let result = extract_args_count(line, 10);
        assert_eq!(result, Some(7));
    }

    #[test]
    fn extract_args_count_absent() {
        let line = "add_filter( 'my_hook', 'callback' )";
        let result = extract_args_count(line, 10);
        assert_eq!(result, None);
    }

    #[test]
    fn count_filter_args_basic() {
        let line = "apply_filters( 'my_hook', $request, $provider, $tools )";
        let count = count_filter_args(line, 13); // after "apply_filters"
        assert_eq!(count, 3); // 3 args after hook name
    }

    #[test]
    fn count_filter_args_single() {
        let line = "apply_filters( 'my_hook', $value )";
        let count = count_filter_args(line, 13);
        assert_eq!(count, 1);
    }

    #[test]
    fn similarity_identical() {
        assert!((similarity_score("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similarity_completely_different() {
        assert!(similarity_score("abc", "xyz") < 0.1);
    }

    #[test]
    fn similarity_partial() {
        // "scheduleTask" vs "scheduleBatch" — share "schedule" prefix + some chars
        let score = similarity_score("scheduleTask", "scheduleBatch");
        assert!(score > 0.5);
        assert!(score < 1.0);
    }

    #[test]
    fn similarity_hook_names() {
        // "datamachine_ai_request" vs "datamachine_ai_request_v2" — clearly related
        let score = similarity_score("datamachine_ai_request", "datamachine_ai_request_v2");
        assert!(score > 0.75);

        // "chubes_ai_request" vs "chubes_ai_models" — same prefix, different suffix
        // Should NOT be flagged as a rename (different hooks)
        let score = similarity_score("chubes_ai_request", "chubes_ai_models");
        assert!(score < 0.75);
    }

    #[test]
    fn hook_similarity_suffix_weighting() {
        // "pre_datamachine_ai_request" vs "pre_chubes_ai_request" — same function,
        // different namespace. Should score HIGHER than prefix-matching alternative.
        let suffix_match = hook_similarity_score(
            "pre_datamachine_ai_request",
            "pre_chubes_ai_request",
        );
        // "pre_datamachine_ai_request" vs "pre_datamachine_directives" — same namespace,
        // different function. Without suffix weighting, this scores higher on LCS.
        let prefix_match = hook_similarity_score(
            "pre_datamachine_ai_request",
            "pre_datamachine_directives",
        );
        // Suffix match should win: shared "_ai_request" suffix indicates same feature.
        assert!(
            suffix_match > prefix_match,
            "suffix_match ({suffix_match:.3}) should beat prefix_match ({prefix_match:.3})"
        );
        // Both should be above threshold to be considered candidates.
        assert!(suffix_match > 0.75);
    }

    #[test]
    fn hook_similarity_rejects_different_functions() {
        // "chubes_ai_request" vs "chubes_ai_models" — different function suffix
        let score = hook_similarity_score("chubes_ai_request", "chubes_ai_models");
        // Should still be below threshold (different features)
        assert!(score < 0.75);
    }

    #[test]
    fn hook_mismatch_exact_match_no_error() {
        let mut index = HookIndex::new();
        index.insert(
            "my_hook".to_string(),
            vec![HookReference {
                name: "my_hook".to_string(),
                file: "inc/Foo.php".to_string(),
                line: 10,
                args_count: None,
                kind: "definition".to_string(),
            }],
        );

        let reg = HookReference {
            name: "my_hook".to_string(),
            file: "tests/FooTest.php".to_string(),
            line: 5,
            args_count: None,
            kind: "registration".to_string(),
        };

        assert!(check_hook_mismatch(&reg, &index).is_none());
    }

    #[test]
    fn hook_mismatch_not_found() {
        let index = HookIndex::new();

        let reg = HookReference {
            name: "nonexistent_hook".to_string(),
            file: "tests/FooTest.php".to_string(),
            line: 5,
            args_count: None,
            kind: "registration".to_string(),
        };

        let result = check_hook_mismatch(&reg, &index);
        assert!(result.is_some());
        assert_eq!(result.unwrap().mismatch_type, MismatchType::HookNotFound);
    }

    #[test]
    fn hook_mismatch_renamed() {
        let mut index = HookIndex::new();
        index.insert(
            "datamachine_ai_request_v2".to_string(),
            vec![HookReference {
                name: "datamachine_ai_request_v2".to_string(),
                file: "inc/AI/RequestBuilder.php".to_string(),
                line: 94,
                args_count: Some(6),
                kind: "definition".to_string(),
            }],
        );

        let reg = HookReference {
            name: "datamachine_ai_request".to_string(),
            file: "tests/AltTextTaskTest.php".to_string(),
            line: 139,
            args_count: Some(7),
            kind: "registration".to_string(),
        };

        let result = check_hook_mismatch(&reg, &index);
        assert!(result.is_some());
        let mismatch = result.unwrap();
        assert_eq!(mismatch.mismatch_type, MismatchType::HookRenamed);
        assert_eq!(
            mismatch.production_symbol,
            Some("datamachine_ai_request_v2".to_string())
        );
        assert!(mismatch.auto_fixable);
    }

    #[test]
    fn hook_prefix_stripping_finds_base_match() {
        // Test registers "pre_datamachine_ai_request" — no production hook matches
        // directly, but after stripping "pre_", "datamachine_ai_request" is similar
        // to production hook "chubes_ai_request" (via suffix-weighted similarity).
        let mut index = HookIndex::new();
        index.insert(
            "chubes_ai_request".to_string(),
            vec![HookReference {
                name: "chubes_ai_request".to_string(),
                file: "inc/AI/RequestBuilder.php".to_string(),
                line: 94,
                args_count: Some(6),
                kind: "definition".to_string(),
            }],
        );
        index.insert(
            "datamachine_directives".to_string(),
            vec![HookReference {
                name: "datamachine_directives".to_string(),
                file: "inc/AI/RequestBuilder.php".to_string(),
                line: 57,
                args_count: None,
                kind: "definition".to_string(),
            }],
        );

        let reg = HookReference {
            name: "pre_datamachine_ai_request".to_string(),
            file: "tests/AltTextTaskTest.php".to_string(),
            line: 139,
            args_count: Some(7),
            kind: "registration".to_string(),
        };

        let result = check_hook_mismatch(&reg, &index);
        assert!(result.is_some());
        let mismatch = result.unwrap();
        assert_eq!(mismatch.mismatch_type, MismatchType::HookRenamed);
        // Should suggest the base hook (without pre_), matched by suffix similarity
        assert_eq!(
            mismatch.production_symbol,
            Some("chubes_ai_request".to_string()),
            "Should match via prefix stripping + suffix similarity, not namespace similarity"
        );
        assert!(mismatch.auto_fixable);
    }

    #[test]
    fn mock_mismatch_exact_match() {
        let mut index = MethodIndex::new();
        index.insert(
            "scheduleBatch".to_string(),
            vec![MethodCall {
                class: "$systemAgent".to_string(),
                method: "scheduleBatch".to_string(),
                file: "inc/Abilities/Media/AltTextAbilities.php".to_string(),
                line: 245,
            }],
        );

        let mock = MockExpectation {
            class: "SystemAgent".to_string(),
            method: "scheduleBatch".to_string(),
            file: "tests/AltTextAbilitiesTest.php".to_string(),
            line: 120,
        };

        assert!(check_mock_mismatch(&mock, &index).is_none());
    }

    #[test]
    fn mock_mismatch_renamed() {
        let mut index = MethodIndex::new();
        index.insert(
            "scheduleBatch".to_string(),
            vec![MethodCall {
                class: "$systemAgent".to_string(),
                method: "scheduleBatch".to_string(),
                file: "inc/Abilities/Media/AltTextAbilities.php".to_string(),
                line: 245,
            }],
        );

        let mock = MockExpectation {
            class: "SystemAgent".to_string(),
            method: "scheduleTask".to_string(),
            file: "tests/AltTextAbilitiesTest.php".to_string(),
            line: 120,
        };

        let result = check_mock_mismatch(&mock, &index);
        assert!(result.is_some());
        let mismatch = result.unwrap();
        assert_eq!(mismatch.mismatch_type, MismatchType::MockMethodRenamed);
        assert_eq!(
            mismatch.production_symbol,
            Some("scheduleBatch".to_string())
        );
        assert!(mismatch.auto_fixable);
    }

    #[test]
    fn fallback_extract_test_file() {
        let content = r#"<?php
class FooTest extends WP_UnitTestCase {
    public function test_hook() {
        add_filter( 'pre_datamachine_ai_request', $callback, 10, 7 );
        $mock->method( 'scheduleTask' );
    }
}
"#;
        let extraction = fallback_extract("tests/FooTest.php", content);
        assert_eq!(extraction.hook_registrations.len(), 1);
        assert_eq!(
            extraction.hook_registrations[0].name,
            "pre_datamachine_ai_request"
        );
        assert_eq!(extraction.mock_expectations.len(), 1);
        assert_eq!(extraction.mock_expectations[0].method, "scheduleTask");
    }

    #[test]
    fn fallback_extract_production_file() {
        let content = r#"<?php
class RequestBuilder {
    public static function build() {
        return apply_filters( 'chubes_ai_request', $request, $provider, $tools );
    }
}
"#;
        let extraction = fallback_extract("inc/AI/RequestBuilder.php", content);
        assert_eq!(extraction.hook_definitions.len(), 1);
        assert_eq!(extraction.hook_definitions[0].name, "chubes_ai_request");
        assert_eq!(extraction.hook_definitions[0].args_count, Some(3));
    }

    #[test]
    fn generate_rules_from_mismatches() {
        let report = CrossRefReport {
            component: "test".into(),
            mismatches: vec![
                CrossRefMismatch {
                    mismatch_type: MismatchType::HookRenamed,
                    test_file: "tests/FooTest.php".into(),
                    test_line: 10,
                    test_symbol: "pre_datamachine_ai_request".into(),
                    production_symbol: Some("chubes_ai_request".into()),
                    production_file: Some("inc/RequestBuilder.php".into()),
                    description: "Hook renamed".into(),
                    auto_fixable: true,
                },
                CrossRefMismatch {
                    mismatch_type: MismatchType::MockMethodRenamed,
                    test_file: "tests/BarTest.php".into(),
                    test_line: 20,
                    test_symbol: "scheduleTask".into(),
                    production_symbol: Some("scheduleBatch".into()),
                    production_file: Some("inc/Abilities.php".into()),
                    description: "Mock renamed".into(),
                    auto_fixable: true,
                },
                CrossRefMismatch {
                    mismatch_type: MismatchType::HookNotFound,
                    test_file: "tests/BazTest.php".into(),
                    test_line: 30,
                    test_symbol: "totally_gone".into(),
                    production_symbol: None,
                    production_file: None,
                    description: "Not found".into(),
                    auto_fixable: false,
                },
            ],
            hook_mismatches: 2,
            mock_mismatches: 1,
            total_auto_fixable: 2,
            test_files_scanned: 3,
            production_files_scanned: 10,
        };

        let rules = generate_transform_rules(&report);
        assert_eq!(rules.len(), 2); // Only auto-fixable ones

        assert_eq!(rules[0].find, "pre_datamachine_ai_request");
        assert_eq!(rules[0].replace, "chubes_ai_request");

        assert_eq!(rules[1].find, r"\bscheduleTask\b");
        assert_eq!(rules[1].replace, "scheduleBatch");
    }

    #[test]
    fn lcs_basic() {
        assert_eq!(longest_common_subsequence_len("abc", "abc"), 3);
        assert_eq!(longest_common_subsequence_len("abc", "axbxc"), 3);
        assert_eq!(longest_common_subsequence_len("abc", "xyz"), 0);
    }

    #[test]
    fn deduplicates_rules() {
        let report = CrossRefReport {
            component: "test".into(),
            mismatches: vec![
                CrossRefMismatch {
                    mismatch_type: MismatchType::HookRenamed,
                    test_file: "tests/A.php".into(),
                    test_line: 1,
                    test_symbol: "old_hook".into(),
                    production_symbol: Some("new_hook".into()),
                    production_file: Some("inc/X.php".into()),
                    description: String::new(),
                    auto_fixable: true,
                },
                CrossRefMismatch {
                    mismatch_type: MismatchType::HookRenamed,
                    test_file: "tests/B.php".into(),
                    test_line: 2,
                    test_symbol: "old_hook".into(),
                    production_symbol: Some("new_hook".into()),
                    production_file: Some("inc/X.php".into()),
                    description: String::new(),
                    auto_fixable: true,
                },
            ],
            hook_mismatches: 2,
            mock_mismatches: 0,
            total_auto_fixable: 2,
            test_files_scanned: 2,
            production_files_scanned: 1,
        };

        let rules = generate_transform_rules(&report);
        assert_eq!(rules.len(), 1); // Deduplicated
    }
}
