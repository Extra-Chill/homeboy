//! helpers — extracted from contract_testgen.rs.

use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use regex::Regex;
use serde::{Deserialize, Serialize};
use super::GeneratedTestOutput;
use super::render_test_plan;
use super::is_path_like;
use super::is_numeric_like;
use super::build_type_registry;
use super::super::contract::*;


/// Derive the template key from the return type shape and the branch's return variant.
pub(crate) fn derive_template_key(return_type: &ReturnShape, returns: &ReturnValue) -> String {
    match return_type {
        ReturnShape::ResultType { .. } => format!("result_{}", returns.variant),
        ReturnShape::OptionType { .. } => format!("option_{}", returns.variant),
        ReturnShape::Bool => format!("bool_{}", returns.variant),
        ReturnShape::Unit => "unit".to_string(),
        ReturnShape::Collection { .. } => "collection".to_string(),
        _ => format!("value_{}", returns.variant),
    }
}

/// Convert a condition string to a snake_case slug suitable for a test name.
pub(crate) fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '.' || c == ':' || c == '-' || c == '_' {
                '_'
            } else {
                '_'
            }
        })

/// Analyze a branch condition to produce a semantic hint for a parameter.
///
/// This is the core of behavioral inference — it recognizes common condition
/// patterns and maps them to language-agnostic hints. The hints are then
/// resolved through the grammar's `type_constructors` to get actual code.
///
/// Returns `None` if no hint can be inferred for this parameter.
pub(crate) fn infer_hint_for_param(condition: &str, condition_lower: &str, param: &Param) -> Option<String> {
    let pname = &param.name;
    let ptype = &param.param_type;

    // ── Negated emptiness — check BEFORE non-negated to avoid false matches ──
    if condition_contains_negated_method(condition, pname, "is_empty") {
        return Some(hints::NON_EMPTY.to_string());
    }

    // ── Emptiness: "param.is_empty()" ──
    if condition_contains_param_method(condition_lower, pname, "is_empty") {
        return Some(hints::EMPTY.to_string());
    }

    // ── Option: "param.is_none()" ──
    if (condition_contains_param_method(condition_lower, pname, "is_none")
        || (condition_lower.contains(&pname.to_lowercase()) && condition_lower.contains("none")))
        && ptype.starts_with("Option")
    {
        return Some(hints::NONE.to_string());
    }

    // ── Option: "param.is_some()" ──
    if (condition_contains_param_method(condition_lower, pname, "is_some")
        || (condition_lower.contains(&pname.to_lowercase()) && condition_lower.contains("some")))
        && ptype.starts_with("Option")
    {
        return Some(hints::SOME_DEFAULT.to_string());
    }

    // ── Path existence ──
    if is_path_like(ptype) {
        if condition_lower.contains("doesn't exist")
            || condition_lower.contains("does not exist")
            || condition_lower.contains("not exist")
            || condition_contains_negated_method(condition, pname, "exists")
        {
            return Some(hints::NONEXISTENT_PATH.to_string());
        }
        if condition_contains_param_method(condition_lower, pname, "exists")
            && !condition_lower.contains("not")
            && !condition.contains('!')
        {
            return Some(hints::EXISTENT_PATH.to_string());
        }
    }

    // ── Boolean params ──
    if ptype.trim() == "bool" {
        if condition_lower.contains(&format!("!{}", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} == false", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} is false", pname.to_lowercase()))
        {
            return Some(hints::FALSE.to_string());
        }
        if condition_lower == pname.to_lowercase()
            || condition_lower.contains(&format!("{} == true", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} is true", pname.to_lowercase()))
        {
            return Some(hints::TRUE.to_string());
        }
    }

    // ── Numeric comparisons ──
    if is_numeric_like(ptype) {
        if condition_lower.contains(&format!("{} == 0", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} < 1", pname.to_lowercase()))
        {
            return Some(hints::ZERO.to_string());
        }
        if condition_lower.contains(&format!("{} > 0", pname.to_lowercase()))
            || condition_lower.contains(&format!("{} >= 1", pname.to_lowercase()))
        {
            return Some(hints::POSITIVE.to_string());
        }
    }

    // ── String content: ".contains(X)" or ".starts_with(X)" ──
    if let Some(literal) = extract_method_string_arg(condition, pname, "contains") {
        // Store the literal in the hint using a separator
        return Some(format!("{}:{}", hints::CONTAINS, literal));
    }
    if let Some(literal) = extract_method_string_arg(condition, pname, "starts_with") {
        return Some(format!("{}:{}", hints::CONTAINS, literal));
    }

    None
}

/// Extract a string literal argument from a method call in a condition.
///
/// E.g., from `name.contains("foo")` extracts `"foo"`.
pub(crate) fn extract_method_string_arg(condition: &str, param: &str, method: &str) -> Option<String> {
    let pattern = format!("{}.{}(\"", param, method);
    if let Some(start) = condition.find(&pattern) {
        let after = &condition[start + pattern.len()..];
        if let Some(end) = after.find('"') {
            return Some(after[..end].to_string());
        }
    }
    // Also try single-quote variant
    let pattern_sq = format!("{}.{}('", param, method);
    if let Some(start) = condition.find(&pattern_sq) {
        let after = &condition[start + pattern_sq.len()..];
        if let Some(end) = after.find('\'') {
            return Some(after[..end].to_string());
        }
    }
    None
}

/// Merge new import lines into existing imports, deduplicating.
pub(crate) fn merge_imports(existing: &str, new_imports: &str) -> String {
    let mut all: Vec<String> = Vec::new();
    for line in existing.lines().chain(new_imports.lines()) {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() && !all.contains(&trimmed) {
            all.push(trimmed);
        }
    }
    all.join("\n")
}

/// Build a project-wide type registry by scanning all source files.
///
/// Walks the project tree via `codebase_scan`, extracts struct/class
/// definitions from each file using grammar items, and parses their fields.
/// Returns a map from type name to `TypeDefinition` spanning the entire project.
///
/// This enables cross-file type resolution: when `validate_write()` returns
/// `Result<ValidationResult, Error>` and `ValidationResult` is defined in a
/// different file, the registry still finds it.
pub fn build_project_type_registry(
    root: &std::path::Path,
    _grammar: &crate::extension::grammar::Grammar,
    contract_grammar: &ContractGrammar,
) -> HashMap<String, TypeDefinition> {
    let mut registry = HashMap::new();

    let field_pattern = match &contract_grammar.field_pattern {
        Some(p) => p.clone(),
        None => {
            crate::log_status!(
                "testgen",
                "Type registry: no field_pattern in contract grammar — skipping"
            );
            return registry;
        }
    };

    // Determine file extensions to scan from the grammar
    let scan_config = crate::engine::codebase_scan::ScanConfig {
        extensions: crate::engine::codebase_scan::ExtensionFilter::All,
        skip_hidden: true,
        ..Default::default()
    };

    let files = crate::engine::codebase_scan::walk_files(root, &scan_config);

    let mut files_scanned = 0usize;
    let mut files_with_grammar = 0usize;

    for file_path in &files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        files_scanned += 1;

        // Check if this file's extension has a matching grammar
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let file_grammar = match crate::code_audit::core_fingerprint::load_grammar_for_ext(ext) {
            Some(g) => g,
            None => continue,
        };
        files_with_grammar += 1;

        // Use the file's own grammar for item extraction (handles multi-language projects)
        let items = crate::extension::grammar_items::parse_items(&content, &file_grammar);
        let symbols = crate::extension::grammar::extract(&content, &file_grammar);

        let mut item_source: HashMap<String, String> = HashMap::new();
        for item in &items {
            if item.kind == "struct" || item.kind == "enum" || item.kind == "class" {
                item_source.insert(item.name.clone(), item.source.clone());
            }
        }

        for sym in &symbols {
            if sym.concept != "struct" && sym.concept != "class" {
                continue;
            }
            let name: String = match sym.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let source = match item_source.get(&name) {
                Some(s) => s,
                None => continue,
            };

            // Use the contract_grammar's field pattern (from the target language)
            let fields = parse_fields_from_source(
                source,
                &field_pattern,
                contract_grammar.field_visibility_pattern.as_deref(),
                contract_grammar.field_name_group,
                contract_grammar.field_type_group,
            );

            let is_public = sym
                .captures
                .get("visibility")
                .map(|v: &String| v.contains("pub"))
                .unwrap_or(false);

            registry.insert(
                name.clone(),
                TypeDefinition {
                    name,
                    kind: sym.concept.clone(),
                    file: rel_path.clone(),
                    line: sym.line,
                    fields,
                    is_public,
                },
            );
        }
    }

    if files_scanned > 0 {
        crate::log_status!(
            "testgen",
            "Type registry: scanned {} files, {} had grammars, found {} types",
            files_scanned,
            files_with_grammar,
            registry.len()
        );
    }

    registry
}

/// Generate tests for specific methods with access to a project-wide type registry.
pub fn generate_tests_for_methods_with_types(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
    method_names: &[&str],
    project_type_registry: Option<&HashMap<String, TypeDefinition>>,
) -> Option<GeneratedTestOutput> {
    let contract_grammar = grammar.contract.as_ref()?;

    if contract_grammar.test_templates.is_empty() {
        return None;
    }

    let contracts =
        super::contract_extract::extract_contracts_from_grammar(content, file_path, grammar)?;

    if contracts.is_empty() {
        return None;
    }

    // Build per-file type registry, then merge with project-wide registry.
    // Same strategy as generate_tests_for_file_with_types — ensures types
    // defined in the current file are always available for enrichment.
    let mut local_registry = build_type_registry(content, file_path, grammar, contract_grammar);

    if let Some(project_reg) = project_type_registry {
        for (name, typedef) in project_reg {
            local_registry
                .entry(name.clone())
                .or_insert_with(|| typedef.clone());
        }
    }

    let type_registry = &local_registry;

    let mut test_source = String::new();
    let mut all_extra_imports: Vec<String> = Vec::new();
    let mut tested_functions = Vec::new();

    for contract in &contracts {
        // Only generate tests for the requested methods
        if !method_names.contains(&contract.name.as_str()) {
            continue;
        }

        let plan = generate_test_plan_with_types(contract, contract_grammar, type_registry);
        if plan.cases.is_empty() {
            continue;
        }

        for case in &plan.cases {
            if let Some(imports_str) = case.variables.get("extra_imports") {
                for imp in imports_str.lines() {
                    let imp = imp.trim().to_string();
                    if !imp.is_empty() && !all_extra_imports.contains(&imp) {
                        all_extra_imports.push(imp);
                    }
                }
            }
        }

        let rendered = render_test_plan(&plan, &contract_grammar.test_templates);
        if !rendered.trim().is_empty() {
            tested_functions.push(contract.name.clone());
            test_source.push_str(&rendered);
        }
    }

    if test_source.trim().is_empty() {
        None
    } else {
        Some(GeneratedTestOutput {
            test_source,
            extra_imports: all_extra_imports,
            tested_functions,
        })
    }
}
