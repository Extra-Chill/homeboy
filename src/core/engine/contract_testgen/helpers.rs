//! helpers — extracted from contract_testgen.rs.

use super::EXISTENT_PATH;
use super::SOME_DEFAULT;
use super::POSITIVE;
use super::is_path_like;
use super::EMPTY;
use super::NON_EMPTY;
use super::TRUE;
use super::NONE;
use super::CONTAINS;
use super::is_numeric_like;
use super::FALSE;
use super::NONEXISTENT_PATH;
use super::ZERO;
use super::super::contract::*;
use super::super::*;


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
        .collect::<String>()
        // Collapse multiple underscores
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
        // Truncate to reasonable length
        .chars()
        .take(60)
        .collect()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_template_key_default_path() {

        let _result = derive_template_key();
    }

    #[test]
    fn test_slugify_default_path() {

        let _result = slugify();
    }

    #[test]
    fn test_infer_hint_for_param_condition_contains_negated_method_condition_pname_is_empty() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_contains_negated_method(condition, pname, \"is_empty\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_contains_param_method_condition_lower_pname_is_emp() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_contains_param_method(condition_lower, pname, \"is_empty\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_contains_param_method_condition_lower_pname_is_non() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: (condition_contains_param_method(condition_lower, pname, \"is_none\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_contains_param_method_condition_lower_pname_is_som() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: (condition_contains_param_method(condition_lower, pname, \"is_some\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_lower_contains_doesn_t_exist() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_lower.contains(\"doesn't exist\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_contains_param_method_condition_lower_pname_exists() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_contains_param_method(condition_lower, pname, \"exists\")");
    }

    #[test]
    fn test_infer_hint_for_param_condition_lower_contains_format_pname_to_lowercase() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_lower.contains(&format!(\"!{{}}\", pname.to_lowercase()))");
    }

    #[test]
    fn test_infer_hint_for_param_condition_lower_pname_to_lowercase() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_lower == pname.to_lowercase()");
    }

    #[test]
    fn test_infer_hint_for_param_condition_lower_contains_format_0_pname_to_lowercase() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_lower.contains(&format!(\"{{}} == 0\", pname.to_lowercase()))");
    }

    #[test]
    fn test_infer_hint_for_param_condition_lower_contains_format_0_pname_to_lowercase_2() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: condition_lower.contains(&format!(\"{{}} > 0\", pname.to_lowercase()))");
    }

    #[test]
    fn test_infer_hint_for_param_if_let_some_literal_extract_method_string_arg_condition_pnam() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: if let Some(literal) = extract_method_string_arg(condition, pname, \"contains\") {{");
    }

    #[test]
    fn test_infer_hint_for_param_let_some_literal_extract_method_string_arg_condition_pname_c() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: let Some(literal) = extract_method_string_arg(condition, pname, \"contains\")");
    }

    #[test]
    fn test_infer_hint_for_param_let_some_literal_extract_method_string_arg_condition_pname_c_2() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: let Some(literal) = extract_method_string_arg(condition, pname, \"contains\")");
    }

    #[test]
    fn test_infer_hint_for_param_let_some_literal_extract_method_string_arg_condition_pname_s() {

        let result = infer_hint_for_param();
        assert!(result.is_some(), "expected Some for: let Some(literal) = extract_method_string_arg(condition, pname, \"starts_with\")");
    }

    #[test]
    fn test_infer_hint_for_param_let_some_literal_extract_method_string_arg_condition_pname_s_2() {

        let result = infer_hint_for_param();
        assert!(result.is_none(), "expected None for: let Some(literal) = extract_method_string_arg(condition, pname, \"starts_with\")");
    }

    #[test]
    fn test_extract_method_string_arg_if_let_some_start_condition_find_pattern() {

        let result = extract_method_string_arg();
        assert!(result.is_some(), "expected Some for: if let Some(start) = condition.find(&pattern) {{");
    }

    #[test]
    fn test_extract_method_string_arg_let_some_start_condition_find_pattern() {

        let result = extract_method_string_arg();
        assert!(result.is_some(), "expected Some for: let Some(start) = condition.find(&pattern)");
    }

    #[test]
    fn test_extract_method_string_arg_let_some_end_after_find() {

        let result = extract_method_string_arg();
        assert!(result.is_some(), "expected Some for: let Some(end) = after.find('\"')");
    }

}
use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::resolve_type_default;
use super::ValidationResult;

/// Like `infer_setup_from_condition` but also applies complement hints
/// for params that aren't matched by the current condition.
pub(crate) fn infer_setup_with_complements(
    condition: &str,
    params: &[Param],
    type_defaults: &[TypeDefault],
    type_constructors: &[TypeConstructor],
    fallback_default: &str,
    complement_hints: &HashMap<String, String>,
) -> Option<SetupOverride> {
    let condition_lower = condition.to_lowercase();

    // Step 1: Produce direct hints from this branch's condition
    let mut param_hints: HashMap<String, String> = HashMap::new();
    for param in params {
        if let Some(hint) = infer_hint_for_param(condition, &condition_lower, param) {
            param_hints.insert(param.name.clone(), hint);
        }
    }

    // Step 2: Apply complement hints for params not directly matched
    for (param_name, complement) in complement_hints {
        if !param_hints.contains_key(param_name) {
            param_hints.insert(param_name.clone(), complement.clone());
        }
    }

    if param_hints.is_empty() {
        return None;
    }

    // Step 3: Resolve all hints through grammar constructors
    let mut setup_lines = Vec::new();
    let mut call_args = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();

    for param in params {
        let (value_expr, call_arg, imports) = if let Some(hint) = param_hints.get(&param.name) {
            resolve_constructor(
                hint,
                &param.name,
                &param.param_type,
                type_constructors,
                type_defaults,
                fallback_default,
            )
        } else {
            let (val, call_override, imps) =
                resolve_type_default(&param.param_type, type_defaults, fallback_default);
            let call =
                call_override.unwrap_or_else(|| default_call_arg(&param.name, &param.param_type));
            let imp_strs: Vec<String> = imps.into_iter().map(|s| s.to_string()).collect();
            (val, call, imp_strs)
        };

        setup_lines.push(format!("        let {} = {};", param.name, value_expr));
        call_args.push(call_arg);

        for imp in imports {
            if !all_imports.contains(&imp) {
                all_imports.push(imp);
            }
        }
    }

    Some(SetupOverride {
        setup_lines: setup_lines.join("\n"),
        call_args: call_args.join(", "),
        extra_imports: all_imports.join("\n"),
    })
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
