//! Test plan generation from function contracts.
//!
//! Takes a `FunctionContract` and produces a `TestPlan` — a structured
//! description of what tests to generate. The plan is language-agnostic.
//!
//! Rendering the plan into actual source code uses templates from grammar.toml
//! `[contract.test_templates]`. Core fills in the variables, the grammar
//! provides the syntax.
//!
//! ## Behavioral inference
//!
//! The test plan generator analyzes branch conditions and return values to
//! produce **behavioral** tests rather than smoke tests. For each branch:
//!
//! 1. **Setup inference** — pattern-matches the condition string against the
//!    parameter types to derive input values that trigger the branch. E.g.,
//!    `"commits.is_empty()"` with a `Vec<CommitInfo>` param → pass `vec![]`.
//!
//! 2. **Assertion inference** — uses the return variant and value description
//!    to derive specific assertions. E.g., `Ok("skipped")` → assert the
//!    unwrapped value matches the expected description, not just `is_ok()`.

mod build;
mod condition_contains;
mod default_call_arg;
mod generate_test;
mod helpers;
mod like;
mod test_async;
mod types;

pub use build::*;
pub use condition_contains::*;
pub use default_call_arg::*;
pub use generate_test::*;
pub use helpers::*;
pub use like::*;
pub use test_async::*;
pub use types::*;


use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::contract::*;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};

/// Resolve a default value expression for a parameter type using type_defaults patterns.
///
/// Returns `(value_expr, call_arg_expr, imports)` where:
/// - `value_expr` is the `let` binding right-hand side (e.g., `String::new()`)
/// - `call_arg_expr` is what to pass in the function call (e.g., `&name` for `&str` params)
/// - `imports` are any extra `use` statements needed
fn resolve_type_default<'a>(
    param_type: &str,
    type_defaults: &'a [TypeDefault],
    fallback_default: &str,
) -> (String, Option<String>, Vec<&'a str>) {
    for td in type_defaults {
        if let Ok(re) = Regex::new(&td.pattern) {
            if re.is_match(param_type) {
                let imports: Vec<&str> = td.imports.iter().map(|s| s.as_str()).collect();
                return (td.value.clone(), None, imports);
            }
        }
    }
    // Fallback: language-specific default from grammar
    (fallback_default.to_string(), None, vec![])
}

// ── Behavioral inference ──
//
// Core analyzes branch conditions to produce **semantic hints** — language-agnostic
// descriptions of what a parameter value should be (e.g., "empty", "none",
// "nonexistent_path"). The grammar's `type_constructors` section then maps
// each (hint, type_pattern) pair to a language-specific code expression.
//
// This keeps core completely language-agnostic. Adding a new language is just
// adding a new grammar file — no core changes needed.

/// Well-known semantic hints that core can produce from condition analysis.
/// These are the "vocabulary" between core and grammar extensions.
///
/// Extensions define `[[contract.type_constructors]]` entries with `hint` fields
/// matching these strings. Extensions may also define custom hints.
mod hints {
    pub const EMPTY: &str = "empty";
    pub const NON_EMPTY: &str = "non_empty";
    pub const NONE: &str = "none";
    pub const SOME_DEFAULT: &str = "some_default";
    pub const NONEXISTENT_PATH: &str = "nonexistent_path";
    pub const EXISTENT_PATH: &str = "existent_path";
    pub const TRUE: &str = "true";
    pub const FALSE: &str = "false";
    pub const ZERO: &str = "zero";
    pub const POSITIVE: &str = "positive";
    pub const CONTAINS: &str = "contains";
}

/// Resolve an assertion for a branch return using grammar-defined assertion templates.
///
/// Core selects an assertion key based on the return type and variant. The grammar's
/// `assertion_templates` section provides language-specific assertion code for each key.
/// Falls back to simple variant-check assertions if no template is found.
fn resolve_assertion(
    returns: &ReturnValue,
    return_type: &ReturnShape,
    condition: &str,
    assertion_templates: &HashMap<String, String>,
) -> String {
    let indent = "        ";

    // Determine the assertion key based on return type + variant + whether we have a value
    let has_value = returns.value.is_some();
    let variant = returns.variant.as_str();

    let key = match return_type {
        ReturnShape::ResultType { .. } => {
            if has_value {
                format!("result_{}_value", variant)
            } else {
                format!("result_{}", variant)
            }
        }
        ReturnShape::OptionType { .. } => {
            if has_value {
                format!("option_{}_value", variant)
            } else {
                format!("option_{}", variant)
            }
        }
        ReturnShape::Bool => format!("bool_{}", variant),
        ReturnShape::Collection { .. } => {
            if condition.contains("empty") || condition.contains("is_empty") {
                "collection_empty".to_string()
            } else {
                "collection_non_empty".to_string()
            }
        }
        _ => "value_default".to_string(),
    };

    // Try the specific key first, then fall back to the base key (without _value)
    let template = assertion_templates.get(&key).or_else(|| {
        // Fall back: result_ok_value → result_ok
        let base = key.rsplit_once('_').map(|(base, _)| base.to_string());
        base.and_then(|b| assertion_templates.get(&b))
    });

    if let Some(tmpl) = template {
        // Substitute variables in the assertion template.
        // Sanitize condition text for embedding inside Rust string literals.
        // Source-level conditions can contain quotes, backticks, and braces
        // (e.g. `Some(format!("```{} block", language))`) that break generated
        // assert messages. Escape quotes and replace braces to avoid format
        // string interpretation in the outer assert! macro.
        let mut rendered = tmpl.clone();
        rendered = rendered.replace("{condition}", &sanitize_for_string_literal(condition));
        if let Some(ref val) = returns.value {
            rendered = rendered.replace("{expected_value}", &val.replace('"', "\\\""));
        }
        rendered = rendered.replace("{variant}", variant);
        rendered
    } else {
        // No grammar template — produce a minimal language-agnostic placeholder
        let escaped_condition = sanitize_for_string_literal(condition);
        format!("{indent}let _ = result; // {variant}: {escaped_condition}")
    }
}

/// When a value-level assertion template couldn't be enriched (type not in registry),
/// fall back to the simpler base assertion that tests the discriminant only.
///
/// For example: `result_ok_value` (has TODO placeholder) → `result_ok` (asserts is_ok()).
/// This produces a real test instead of a dead stub.
fn fallback_to_simple_assertion(
    returns: &ReturnValue,
    return_type: &ReturnShape,
    condition: &str,
    assertion_templates: &HashMap<String, String>,
) -> Option<String> {
    let variant = returns.variant.as_str();

    // Build the base key (without _value suffix)
    let base_key = match return_type {
        ReturnShape::ResultType { .. } => format!("result_{}", variant),
        ReturnShape::OptionType { .. } => format!("option_{}", variant),
        _ => return None, // Bool/Collection/etc don't have _value variants
    };

    let tmpl = assertion_templates.get(&base_key)?;

    let mut rendered = tmpl.clone();
    rendered = rendered.replace("{condition}", &sanitize_for_string_literal(condition));
    if let Some(ref val) = returns.value {
        rendered = rendered.replace("{expected_value}", &val.replace('"', "\\\""));
    }
    rendered = rendered.replace("{variant}", variant);
    Some(rendered)
}

/// Sanitize a source-level string for safe embedding inside a Rust string literal.
///
/// Escapes double quotes and replaces curly braces with Unicode lookalikes
/// so the text doesn't interfere with `format!` / `assert!` macro parsing.
/// Backticks in groups of 3+ are replaced to avoid raw string prefix confusion.
fn sanitize_for_string_literal(s: &str) -> String {
    s.replace('"', "\\\"")
        .replace('{', "{{")
        .replace('}', "}}")
        .replace("```", "'''")
}

/// Replace assertion TODO placeholders with real field-level assertions.
///
/// When the return type is a known struct, generates `assert_eq!` for each
/// public field using the field's type-default value as the expected value.
/// This produces tests that actually **assert behavior**, not just document
/// what fields exist.
///
/// Turns:
///   `let _ = inner; // TODO: assert specific value for "skipped"`
/// Into:
///   `assert_eq!(inner.success, false);`
///   `assert_eq!(inner.command, None);`
///   `assert_eq!(inner.rolled_back, false);`
///   `assert_eq!(inner.files_checked, 0);`
fn enrich_assertion_with_fields(
    assertion: &str,
    returns: &ReturnValue,
    return_type: &ReturnShape,
    type_registry: &HashMap<String, TypeDefinition>,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
    field_assertion_template: Option<&str>,
) -> String {
    // Only enrich if the assertion has a TODO placeholder
    if !assertion.contains("TODO:") {
        return assertion.to_string();
    }

    // Determine the inner type name to look up
    let type_name = match return_type {
        ReturnShape::ResultType { ok_type, .. } => {
            if returns.variant == "ok" {
                Some(ok_type.as_str())
            } else {
                None
            }
        }
        ReturnShape::OptionType { some_type } => {
            if returns.variant == "some" {
                Some(some_type.as_str())
            } else {
                None
            }
        }
        ReturnShape::Value { value_type } => Some(value_type.as_str()),
        _ => None,
    };

    let type_name = match type_name {
        Some(n) => n.trim(),
        None => return assertion.to_string(),
    };

    let base_name = type_name
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();

    // Try exact match first, then strip path prefixes (e.g., "crate::engine::ValidationResult" → "ValidationResult")
    let type_def = match type_registry.get(base_name) {
        Some(td) => td,
        None => {
            // Strip Rust path prefix — registry stores bare names
            let short_name = base_name.rsplit("::").next().unwrap_or(base_name);
            match type_registry.get(short_name) {
                Some(td) => td,
                None => return assertion.to_string(),
            }
        }
    };

    let public_fields: Vec<&FieldDef> = type_def.fields.iter().filter(|f| f.is_public).collect();
    if public_fields.is_empty() {
        return assertion.to_string();
    }

    // Must have a field assertion template from the grammar to generate assertions
    let template = match field_assertion_template {
        Some(t) => t,
        None => return assertion.to_string(),
    };

    let indent = "        ";

    // Find the TODO line and everything after it (including the `let _ = inner;` line before it)
    let todo_pos = match assertion.find("// TODO:") {
        Some(pos) => pos,
        None => return assertion.to_string(),
    };

    // Find the start of the line containing the `let _ =` before the TODO
    // We want to replace both the `let _ = inner;` line AND the TODO line
    let search_region = &assertion[..todo_pos];
    let let_underscore_pos = search_region.rfind("let _ = ");
    let replace_start = if let Some(lpos) = let_underscore_pos {
        // Find the line start before `let _ =`
        assertion[..lpos].rfind('\n').map(|p| p + 1).unwrap_or(0)
    } else {
        // No `let _ =` found, just replace from the TODO line start
        assertion[..todo_pos]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0)
    };

    let replace_end = assertion[todo_pos..]
        .find('\n')
        .map(|p| todo_pos + p + 1)
        .unwrap_or(assertion.len());

    // Build real field assertions using the grammar's field_assertion_template
    let mut field_assertions = Vec::new();
    for field in &public_fields {
        let expected = default_for_field_type(&field.field_type, type_defaults, fallback_default);
        let rendered = template
            .replace("{indent}", indent)
            .replace("{field_name}", &field.name)
            .replace("{expected_value}", &expected.replace('"', "\\\""));
        field_assertions.push(rendered);
    }

    format!(
        "{}{}{}",
        &assertion[..replace_start],
        field_assertions.join("\n"),
        &assertion[replace_end..],
    )
}

/// Resolve a default/zero value for a field type to use as expected assertion value.
///
/// Uses the grammar's `type_defaults` exclusively — no language-specific fallbacks
/// in core. If no `type_default` matches, uses `fallback_default` from the grammar.
fn default_for_field_type(
    field_type: &str,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> String {
    let trimmed = field_type.trim();

    for td in type_defaults {
        if let Ok(re) = Regex::new(&td.pattern) {
            if re.is_match(trimmed) {
                return td.value.clone();
            }
        }
    }

    fallback_default.to_string()
}

/// Build complement hints for a branch by examining what other branches require.
///
/// If branch 1 says param X needs hint `"empty"`, then branches that don't
/// mention param X should use `"non_empty"` to reach a different code path.
/// This ensures each branch's test uses inputs that actually trigger that branch.
fn build_complement_hints(
    branch_index: usize,
    all_branch_hints: &[HashMap<String, String>],
) -> HashMap<String, String> {
    let mut complements = HashMap::new();
    let my_hints = &all_branch_hints[branch_index];

    for (j, other_hints) in all_branch_hints.iter().enumerate() {
        if j == branch_index {
            continue;
        }
        for (param_name, hint) in other_hints {
            // Only provide a complement if this branch doesn't have its own hint
            if my_hints.contains_key(param_name) || complements.contains_key(param_name) {
                continue;
            }
            if let Some(complement) = complement_hint(hint) {
                complements.insert(param_name.clone(), complement);
            }
        }
    }

    complements
}

/// Return the opposite hint for a given hint.
///
/// This allows branches that don't mention a param to use the inverse
/// of what another branch requires, ensuring the test reaches the right path.
fn complement_hint(hint: &str) -> Option<String> {
    // Split compound hints like "contains:foo"
    let base = hint.split(':').next().unwrap_or(hint);

    match base {
        "empty" => Some(hints::NON_EMPTY.to_string()),
        "non_empty" => Some(hints::EMPTY.to_string()),
        "none" => Some(hints::SOME_DEFAULT.to_string()),
        "some_default" => Some(hints::NONE.to_string()),
        "true" => Some(hints::FALSE.to_string()),
        "false" => Some(hints::TRUE.to_string()),
        "zero" => Some(hints::POSITIVE.to_string()),
        "positive" => Some(hints::ZERO.to_string()),
        "nonexistent_path" => Some(hints::EXISTENT_PATH.to_string()),
        "existent_path" => Some(hints::NONEXISTENT_PATH.to_string()),
        _ => None,
    }
}

/// Like `infer_setup_from_condition` but also applies complement hints
/// for params that aren't matched by the current condition.
fn infer_setup_with_complements(
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
fn merge_imports(existing: &str, new_imports: &str) -> String {
    let mut all: Vec<String> = Vec::new();
    for line in existing.lines().chain(new_imports.lines()) {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() && !all.contains(&trimmed) {
            all.push(trimmed);
        }
    }
    all.join("\n")
}

// ── Type registry ──

/// Build a type registry from struct/class definitions found in a source file.
///
/// Uses the grammar's symbol extraction to find struct/enum/class definitions,
/// then parses their field declarations using the grammar's `field_pattern`.
/// Returns a map from type name to `TypeDefinition`.
fn build_type_registry(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
    contract_grammar: &ContractGrammar,
) -> HashMap<String, TypeDefinition> {
    let mut registry = HashMap::new();

    // Need a field pattern to parse fields
    let field_pattern = match &contract_grammar.field_pattern {
        Some(p) => p.as_str(),
        None => return registry,
    };

    // Extract all symbols from the file via grammar
    let symbols = crate::extension::grammar::extract(content, grammar);

    // Also extract grammar items to get the full source of structs
    let items = crate::extension::grammar_items::parse_items(content, grammar);

    // Build a lookup from name → source body
    let mut item_source: HashMap<String, String> = HashMap::new();
    for item in &items {
        if item.kind == "struct" || item.kind == "enum" || item.kind == "class" {
            item_source.insert(item.name.clone(), item.source.clone());
        }
    }

    // Process each struct/enum/class symbol
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

        let fields = parse_fields_from_source(
            source,
            field_pattern,
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
                file: file_path.to_string(),
                line: sym.line,
                fields,
                is_public,
            },
        );
    }

    registry
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

// ── End-to-end API ──

/// Generated test output with source code and metadata.
pub struct GeneratedTestOutput {
    /// The rendered test source code (test functions only, no module wrapper).
    pub test_source: String,
    /// Extra `use` imports needed by the generated default values.
    pub extra_imports: Vec<String>,
    /// The function names that tests were generated for.
    pub tested_functions: Vec<String>,
}

/// Generate test source code for all functions in a source file.
///
/// This is the full pipeline: grammar → contracts → test plans → rendered source.
/// Returns `None` if the grammar has no contract or test_templates section.
///
/// When `project_type_registry` is provided, return types from any file in the
/// project can be resolved to their struct fields. When `None`, falls back to
/// a per-file registry (only finds types defined in the same file).
pub(crate) fn generate_tests_for_file(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
) -> Option<GeneratedTestOutput> {
    generate_tests_for_file_with_types(content, file_path, grammar, None)
}

/// Generate test source with access to a project-wide type registry.
pub fn generate_tests_for_file_with_types(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
    project_type_registry: Option<&HashMap<String, TypeDefinition>>,
) -> Option<GeneratedTestOutput> {
    let contract_grammar = grammar.contract.as_ref()?;

    // Must have test templates to render
    if contract_grammar.test_templates.is_empty() {
        return None;
    }

    // Extract contracts
    let contracts =
        super::contract_extract::extract_contracts_from_grammar(content, file_path, grammar)?;

    if contracts.is_empty() {
        return None;
    }

    // Build per-file type registry, then merge with project-wide registry.
    // This ensures types defined in the current file are always available
    // for assertion enrichment, even if the project-wide scan missed them
    // (e.g., due to extension loading issues in CI environments).
    let mut local_registry = build_type_registry(content, file_path, grammar, contract_grammar);

    // Merge project-wide types into local (local takes precedence for same-file types)
    if let Some(project_reg) = project_type_registry {
        for (name, typedef) in project_reg {
            local_registry
                .entry(name.clone())
                .or_insert_with(|| typedef.clone());
        }
    }

    let type_registry = &local_registry;

    // Generate and render test plans
    let mut test_source = String::new();
    let mut all_extra_imports: Vec<String> = Vec::new();
    let mut tested_functions = Vec::new();

    for contract in &contracts {
        // Skip test functions, private functions, and trivial functions
        if contract.name.starts_with("test_") {
            continue;
        }
        if !contract.signature.is_public {
            continue;
        }

        let plan = generate_test_plan_with_types(contract, contract_grammar, type_registry);
        if plan.cases.is_empty() {
            continue;
        }

        // Collect extra imports from case variables
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

/// Generate test source code for specific methods in a source file.
///
/// Like `generate_tests_for_file`, but only generates tests for functions
/// whose names are in `method_names`. Used for MissingTestMethod findings
/// where the test file exists but specific methods lack coverage.
pub(crate) fn generate_tests_for_methods(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
    method_names: &[&str],
) -> Option<GeneratedTestOutput> {
    generate_tests_for_methods_with_types(content, file_path, grammar, method_names, None)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result_contract() -> FunctionContract {
        FunctionContract {
            name: "validate_write".to_string(),
            file: "src/core/engine/validate_write.rs".to_string(),
            line: 86,
            signature: Signature {
                params: vec![
                    Param {
                        name: "root".to_string(),
                        param_type: "&Path".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                    Param {
                        name: "changed_files".to_string(),
                        param_type: "&[PathBuf]".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                ],
                return_type: ReturnShape::ResultType {
                    ok_type: "ValidationResult".to_string(),
                    err_type: "Error".to_string(),
                },
                receiver: None,
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "changed_files.is_empty()".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("skipped".to_string()),
                    },
                    effects: vec![],
                    line: Some(91),
                },
                Branch {
                    condition: "validation command fails".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("failed".to_string()),
                    },
                    effects: vec![Effect::ProcessSpawn {
                        command: Some("sh".to_string()),
                    }],
                    line: Some(130),
                },
            ],
            early_returns: 2,
            effects: vec![Effect::ProcessSpawn {
                command: Some("sh".to_string()),
            }],
            calls: vec![],
            impl_type: None,
        }
    }

    fn sample_option_contract() -> FunctionContract {
        FunctionContract {
            name: "find_item".to_string(),
            file: "src/lib.rs".to_string(),
            line: 10,
            signature: Signature {
                params: vec![Param {
                    name: "key".to_string(),
                    param_type: "&str".to_string(),
                    mutable: false,
                    has_default: false,
                }],
                return_type: ReturnShape::OptionType {
                    some_type: "Item".to_string(),
                },
                receiver: Some(Receiver::Ref),
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "key found".to_string(),
                    returns: ReturnValue {
                        variant: "some".to_string(),
                        value: Some("item".to_string()),
                    },
                    effects: vec![],
                    line: Some(15),
                },
                Branch {
                    condition: "key not found".to_string(),
                    returns: ReturnValue {
                        variant: "none".to_string(),
                        value: None,
                    },
                    effects: vec![],
                    line: Some(20),
                },
            ],
            early_returns: 0,
            effects: vec![],
            calls: vec![],
            impl_type: None,
        }
    }

    fn sample_type_defaults() -> Vec<TypeDefault> {
        vec![
            TypeDefault {
                pattern: r"^&str$".to_string(),
                value: r#""""#.to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^&Path$".to_string(),
                value: r#"Path::new("")"#.to_string(),
                imports: vec!["use std::path::Path;".to_string()],
            },
            TypeDefault {
                pattern: r"^&\[.*\]$".to_string(),
                value: "Vec::new()".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^bool$".to_string(),
                value: "false".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^Option<.*>$".to_string(),
                value: "None".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^usize$|^u\d+$|^i\d+$".to_string(),
                value: "0".to_string(),
                imports: vec![],
            },
            TypeDefault {
                pattern: r"^String$".to_string(),
                value: "String::new()".to_string(),
                imports: vec![],
            },
        ]
    }

    /// Build a ContractGrammar with type_defaults populated.
    fn grammar_with_defaults() -> ContractGrammar {
        ContractGrammar {
            type_defaults: sample_type_defaults(),
            ..Default::default()
        }
    }

    fn sample_test_templates() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("result_ok".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("result_err".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("option_some".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("option_none".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("bool_true".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("bool_false".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let result = {fn_name}({param_args});\n{assertion_code}\n    }}\n".to_string());
        m.insert("no_panic".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let _ = {fn_name}({param_args});\n    }}\n".to_string());
        m.insert("effects".to_string(),
            "    #[test]\n    fn {test_name}() {{\n        // Expected effects: {effects}\n{param_setup}\n        let _ = {fn_name}({param_args});\n    }}\n".to_string());
        m.insert("default".to_string(),
            "    #[test]\n    fn {test_name}() {{\n{param_setup}\n        let _result = {fn_name}({param_args});\n    }}\n".to_string());
        m
    }

    fn sample_type_constructors() -> Vec<TypeConstructor> {
        vec![
            TypeConstructor {
                hint: "empty".to_string(),
                pattern: r"^&?\[.*\]$|^Vec<.*>$".to_string(),
                value: "Vec::new()".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "empty".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: r#""""#.to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "non_empty".to_string(),
                pattern: r"^&?\[.*\]$|^Vec<.*>$".to_string(),
                value: "vec![Default::default()]".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "non_empty".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: "\"test_{param_name}\"".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "nonexistent_path".to_string(),
                pattern: r"(?i)path".to_string(),
                value: r#"Path::new("/tmp/nonexistent_test_path_818")"#.to_string(),
                call_arg: None,
                imports: vec!["use std::path::Path;".to_string()],
            },
            TypeConstructor {
                hint: "none".to_string(),
                pattern: r"^Option<.*>$".to_string(),
                value: "None".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "some_default".to_string(),
                pattern: r"^Option<.*>$".to_string(),
                value: "Some(Default::default())".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "true".to_string(),
                pattern: r"^bool$".to_string(),
                value: "true".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "false".to_string(),
                pattern: r"^bool$".to_string(),
                value: "false".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "zero".to_string(),
                pattern: r"^u\w+$|^i\w+$|^f\w+$".to_string(),
                value: "0".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "positive".to_string(),
                pattern: r"^u\w+$|^i\w+$|^f\w+$".to_string(),
                value: "1".to_string(),
                call_arg: None,
                imports: vec![],
            },
            TypeConstructor {
                hint: "contains".to_string(),
                pattern: r"^&str$|^String$|^&String$".to_string(),
                value: "\"{hint_arg}\"".to_string(),
                call_arg: None,
                imports: vec![],
            },
        ]
    }

    fn sample_assertion_templates() -> HashMap<String, String> {
        let indent = "        ";
        let mut m = HashMap::new();
        m.insert(
            "result_ok".to_string(),
            format!("{indent}assert!(result.is_ok(), \"expected Ok for: {{condition}}\");"),
        );
        m.insert(
            "result_ok_value".to_string(),
            format!(
                "{indent}let inner = result.unwrap();\n\
             {indent}// Branch returns Ok({{expected_value}}) when: {{condition}}\n\
             {indent}let _ = inner; // TODO: assert specific value for \"{{expected_value}}\""
            ),
        );
        m.insert(
            "result_err".to_string(),
            format!("{indent}assert!(result.is_err(), \"expected Err for: {{condition}}\");"),
        );
        m.insert(
            "result_err_value".to_string(),
            format!(
                "{indent}let err = result.unwrap_err();\n\
             {indent}// Branch returns Err({{expected_value}}) when: {{condition}}\n\
             {indent}let err_msg = format!(\"{{{{:?}}}}\", err);\n\
             {indent}let _ = err_msg; // TODO: assert error contains \"{{expected_value}}\""
            ),
        );
        m.insert(
            "option_some".to_string(),
            format!("{indent}assert!(result.is_some(), \"expected Some for: {{condition}}\");"),
        );
        m.insert(
            "option_some_value".to_string(),
            format!(
                "{indent}let inner = result.expect(\"expected Some for: {{condition}}\");\n\
             {indent}// Branch returns Some({{expected_value}})\n\
             {indent}let _ = inner; // TODO: assert value matches \"{{expected_value}}\""
            ),
        );
        m.insert(
            "option_none".to_string(),
            format!("{indent}assert!(result.is_none(), \"expected None for: {{condition}}\");"),
        );
        m.insert(
            "bool_true".to_string(),
            format!("{indent}assert!(result, \"expected true when: {{condition}}\");"),
        );
        m.insert(
            "bool_false".to_string(),
            format!("{indent}assert!(!result, \"expected false when: {{condition}}\");"),
        );
        m.insert(
            "collection_empty".to_string(),
            format!(
            "{indent}assert!(result.is_empty(), \"expected empty collection for: {{condition}}\");"
        ),
        );
        m.insert("collection_non_empty".to_string(), format!(
            "{indent}assert!(!result.is_empty(), \"expected non-empty collection for: {{condition}}\");"
        ));
        m
    }

    #[test]
    fn test_plan_generates_one_case_per_branch() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        // 2 branches + 1 effect test
        assert_eq!(plan.cases.len(), 3);
        assert_eq!(plan.function_name, "validate_write");
    }

    #[test]
    fn test_plan_names_are_descriptive() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert!(plan.cases[0].test_name.starts_with("test_validate_write_"));
        assert!(plan.cases[0].test_name.contains("empty"));
    }

    #[test]
    fn test_plan_template_keys_match_return_shape() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases[0].template_key, "result_ok");
        assert_eq!(plan.cases[1].template_key, "result_ok");
    }

    #[test]
    fn test_plan_for_option_type() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases.len(), 2);
        assert_eq!(plan.cases[0].template_key, "option_some");
        assert_eq!(plan.cases[1].template_key, "option_none");
    }

    #[test]
    fn test_plan_pure_function_no_effect_test() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        // Pure function — no effect test case
        assert!(plan.cases.iter().all(|c| c.template_key != "effects"));
    }

    #[test]
    fn test_plan_variables_contain_fn_info() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());
        let vars = &plan.cases[0].variables;
        assert_eq!(vars.get("fn_name").unwrap(), "validate_write");
        assert_eq!(vars.get("param_names").unwrap(), "root, changed_files");
        assert_eq!(vars.get("return_shape").unwrap(), "result");
    }

    #[test]
    fn test_plan_with_type_defaults_generates_param_setup() {
        let contract = sample_result_contract();
        let grammar = grammar_with_defaults();
        let plan = generate_test_plan(&contract, &grammar);
        let vars = &plan.cases[0].variables;

        let param_setup = vars.get("param_setup").unwrap();
        assert!(
            param_setup.contains("let root ="),
            "should have root binding"
        );
        assert!(
            param_setup.contains("let changed_files ="),
            "should have changed_files binding"
        );
        assert!(
            param_setup.contains("Path::new"),
            "should use Path::new for &Path"
        );
        assert!(
            param_setup.contains("Vec::new()"),
            "should use Vec::new() for &[PathBuf]"
        );

        let param_args = vars.get("param_args").unwrap();
        assert!(
            param_args.contains("&root"),
            "should borrow root for &Path param"
        );
        assert!(
            param_args.contains("&changed_files"),
            "should borrow changed_files for &[PathBuf] param"
        );

        let extra_imports = vars.get("extra_imports").unwrap();
        assert!(
            extra_imports.contains("use std::path::Path;"),
            "should include Path import"
        );
    }

    #[test]
    fn test_render_test_plan_with_templates() {
        let contract = sample_result_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());

        let mut templates = HashMap::new();
        templates.insert(
            "result_ok".to_string(),
            "#[test]\nfn {test_name}() {{\n    // {fn_name} should return Ok\n}}\n".to_string(),
        );

        let output = render_test_plan(&plan, &templates);
        assert!(output.contains("#[test]"));
        assert!(output.contains("test_validate_write_"));
        assert!(output.contains("validate_write should return Ok"));
    }

    #[test]
    fn test_render_test_plan_missing_template_uses_default() {
        let contract = sample_option_contract();
        let plan = generate_test_plan(&contract, &empty_grammar());

        let mut templates = HashMap::new();
        templates.insert(
            "default".to_string(),
            "#[test]\nfn {test_name}() {{ todo!() }}\n".to_string(),
        );

        let output = render_test_plan(&plan, &templates);
        assert!(output.contains("test_find_item_"));
        assert!(output.contains("todo!()"));
    }

    #[test]
    fn test_render_test_plan_deduplicates_identical_names() {
        // Simulate two branches with identical slugified conditions
        let plan = TestPlan {
            function_name: "check_status".to_string(),
            source_file: "src/lib.rs".to_string(),
            is_async: false,
            cases: vec![
                TestCase {
                    test_name: "test_check_status_none_return_false".to_string(),
                    branch_condition: "None => return false".to_string(),
                    expected_variant: "ok".to_string(),
                    expected_value: None,
                    template_key: "default".to_string(),
                    variables: HashMap::new(),
                },
                TestCase {
                    test_name: "test_check_status_none_return_false".to_string(),
                    branch_condition: "None => return false (other arm)".to_string(),
                    branch_condition: "None => return false (third arm)".to_string(),
                    expected_variant: "ok".to_string(),
                    expected_value: None,
                    template_key: "default".to_string(),
                    variables: HashMap::new(),
                },
            ],
        };

        let mut templates = HashMap::new();
        templates.insert("default".to_string(), "fn {test_name}() {{}}\n".to_string());

        let output = render_test_plan(&plan, &templates);

        // First occurrence keeps original name
        assert!(output.contains("fn test_check_status_none_return_false()"));
        // Subsequent occurrences get numeric suffixes
        assert!(output.contains("fn test_check_status_none_return_false_2()"));
        assert!(output.contains("fn test_check_status_none_return_false_3()"));
    }

    #[test]
    fn test_slugify() {
        assert_eq!(
            slugify("changed_files.is_empty()"),
            "changed_files_is_empty"
        );
        assert_eq!(
            slugify("validation command fails"),
            "validation_command_fails"
        );
        assert_eq!(slugify("default path"), "default_path");
    }

    #[test]
    fn test_empty_branches_generates_no_panic_test() {
        let mut contract = sample_result_contract();
        contract.branches.clear();
        contract.effects.clear();
        let plan = generate_test_plan(&contract, &empty_grammar());
        assert_eq!(plan.cases.len(), 1);
        assert_eq!(plan.cases[0].template_key, "no_panic");
    }

    // ── Behavioral inference tests ──

    #[test]
    fn test_setup_infers_empty_vec_for_is_empty_condition() {
        let params = vec![Param {
            name: "items".to_string(),
            param_type: "Vec<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "items.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Vec::new()"),
            "should use Vec::new() for empty vec, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_empty_slice_for_is_empty_condition() {
        let params = vec![Param {
            name: "changed_files".to_string(),
            param_type: "&[PathBuf]".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "changed_files.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for slice is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Vec::new()"),
            "should use Vec::new() for empty slice, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_non_empty_vec_for_negated_is_empty() {
        let params = vec![Param {
            name: "commits".to_string(),
            param_type: "Vec<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "!commits.is_empty()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for negated is_empty");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("vec![Default::default()]"),
            "should use vec![Default::default()] for non-empty vec, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_nonexistent_path() {
        let params = vec![Param {
            name: "path".to_string(),
            param_type: "&Path".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "path doesn't exist",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for nonexistent path");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("nonexistent"),
            "should use a nonexistent path, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_none_for_option_is_none() {
        let params = vec![Param {
            name: "config".to_string(),
            param_type: "Option<Config>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "config.is_none()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_none");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("None"),
            "should use None for is_none, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_infers_some_for_option_is_some() {
        let params = vec![Param {
            name: "config".to_string(),
            param_type: "Option<String>".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "config.is_some()",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some(), "should infer setup for is_some");
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("Some("),
            "should use Some(...) for is_some, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_returns_none_for_unrecognized_condition() {
        let params = vec![Param {
            name: "x".to_string(),
            param_type: "CustomType".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "some random condition",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(
            result.is_none(),
            "should return None for unrecognized condition"
        );
    }

    #[test]
    fn test_setup_preserves_non_overridden_params() {
        let type_defaults = sample_type_defaults();
        let params = vec![
            Param {
                name: "root".to_string(),
                param_type: "&Path".to_string(),
                mutable: false,
                has_default: false,
            },
            Param {
                name: "items".to_string(),
                param_type: "&[String]".to_string(),
                mutable: false,
                has_default: false,
            },
        ];
        // Condition only targets items, root should keep its type_default
        let result = infer_setup_from_condition(
            "items.is_empty()",
            &params,
            &type_defaults,
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("let root = Path::new"),
            "root should keep its type_default, got: {}",
            so.setup_lines
        );
        assert!(
            so.setup_lines.contains("let items = Vec::new()"),
            "items should be overridden to Vec::new(), got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_assertion_result_ok_with_value() {
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("skipped".to_string()),
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "ValidationResult".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "items.is_empty()",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("unwrap()"),
            "should unwrap Ok value, got: {}",
            assertion
        );
        assert!(
            assertion.contains("skipped"),
            "should mention the expected value, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_result_err_with_value() {
        let returns = ReturnValue {
            variant: "err".to_string(),
            value: Some("not found".to_string()),
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "String".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "path doesn't exist",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("unwrap_err()"),
            "should unwrap Err, got: {}",
            assertion
        );
        assert!(
            assertion.contains("not found"),
            "should mention the error description, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_result_ok_without_value_falls_back() {
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: None,
        };
        let return_type = ReturnShape::ResultType {
            ok_type: "()".to_string(),
            err_type: "Error".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "default path",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_ok()"),
            "should fall back to is_ok(), got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_option_none() {
        let returns = ReturnValue {
            variant: "none".to_string(),
            value: None,
        };
        let return_type = ReturnShape::OptionType {
            some_type: "Item".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "key not found",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_none()"),
            "should assert is_none(), got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_escapes_inner_quotes_in_condition() {
        // Regression: conditions like `Some("changed")` contain double quotes
        // that must be escaped when embedded inside generated string literals.
        let returns = ReturnValue {
            variant: "some".to_string(),
            value: Some("changed".to_string()),
        };
        let return_type = ReturnShape::OptionType {
            some_type: "String".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            r#"CommitCategory::Other => Some("changed"),"#,
            &sample_assertion_templates(),
        );
        // The generated code must not contain unescaped inner quotes that would
        // break compilation. Raw `"changed"` inside a string literal is invalid.
        assert!(
            !assertion.contains(r#"Some("changed")"#),
            "inner quotes must be escaped in generated code, got:\n{}",
            assertion
        );
        // Escaped form should be present instead
        assert!(
            assertion.contains(r#"Some(\"changed\")"#),
            "should contain escaped quotes, got:\n{}",
            assertion
        );
    }

    #[test]
    fn test_assertion_bool_true() {
        let returns = ReturnValue {
            variant: "true".to_string(),
            value: None,
        };
        let return_type = ReturnShape::Bool;
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "input is valid",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("assert!(result"),
            "should assert true, got: {}",
            assertion
        );
        assert!(
            !assertion.contains("!result"),
            "should NOT negate, got: {}",
            assertion
        );
    }

    #[test]
    fn test_assertion_collection_empty_condition() {
        let returns = ReturnValue {
            variant: "value".to_string(),
            value: None,
        };
        let return_type = ReturnShape::Collection {
            element_type: "String".to_string(),
        };
        let assertion = resolve_assertion(
            &returns,
            &return_type,
            "input.is_empty()",
            &sample_assertion_templates(),
        );
        assert!(
            assertion.contains("is_empty()"),
            "should assert emptiness, got: {}",
            assertion
        );
    }

    #[test]
    fn test_behavioral_plan_overrides_setup_for_is_empty_branch() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // First branch: changed_files.is_empty()
        let vars = &plan.cases[0].variables;
        let setup = vars.get("param_setup").unwrap();
        assert!(
            setup.contains("let changed_files = Vec::new()"),
            "should override changed_files to empty vec for is_empty branch, got:\n{}",
            setup
        );
        // root should still use its type_default (Path::new)
        assert!(
            setup.contains("let root = Path::new"),
            "root should keep its type_default, got:\n{}",
            setup
        );
    }

    #[test]
    fn test_behavioral_plan_generates_assertion_code() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // First branch returns Ok("skipped") — should have rich assertion
        let vars = &plan.cases[0].variables;
        let assertion = vars.get("assertion_code").unwrap();
        assert!(
            assertion.contains("unwrap()") || assertion.contains("is_ok()"),
            "should have an assertion for Ok branch, got:\n{}",
            assertion
        );
    }

    #[test]
    fn test_setup_bool_param_true() {
        let params = vec![Param {
            name: "verbose".to_string(),
            param_type: "bool".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "verbose == true",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("true"),
            "should set bool to true, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_numeric_param_zero() {
        let params = vec![Param {
            name: "count".to_string(),
            param_type: "usize".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "count == 0",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("let count = 0"),
            "should set count to 0, got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_setup_string_contains() {
        let params = vec![Param {
            name: "name".to_string(),
            param_type: "&str".to_string(),
            mutable: false,
            has_default: false,
        }];
        let result = infer_setup_from_condition(
            "name.contains(\"test\")",
            &params,
            &[],
            &sample_type_constructors(),
            "Default::default()",
        );
        assert!(result.is_some());
        let so = result.unwrap();
        assert!(
            so.setup_lines.contains("\"test\""),
            "should use the literal from contains(), got: {}",
            so.setup_lines
        );
    }

    #[test]
    fn test_cross_branch_complement_gives_non_empty_to_other_branch() {
        // Branch 1: changed_files.is_empty() → hint "empty"
        // Branch 2: "validation command fails" → no hint for changed_files
        // Branch 2 should get the COMPLEMENT: "non_empty" for changed_files
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);

        // Branch 2 (index 1): "validation command fails"
        let vars = &plan.cases[1].variables;
        let setup = vars.get("param_setup").unwrap();
        assert!(
            setup.contains("vec![Default::default()]")
                || setup.contains("vec![")
                || !setup.contains("Vec::new()"),
            "branch 2 should get non-empty changed_files (complement of branch 1's 'empty'), got:\n{}",
            setup
        );
    }

    #[test]
    fn test_full_pipeline_renders_with_behavioral_assertions() {
        let contract = sample_result_contract();
        let grammar = full_grammar();
        let plan = generate_test_plan(&contract, &grammar);
        let rendered = render_test_plan(&plan, &grammar.test_templates);

        // Should produce output
        assert!(!rendered.is_empty(), "rendered output should not be empty");

        // Without type registry, enrichment can't resolve struct fields so the
        // result_ok_value assertion template has a // TODO placeholder. The pipeline
        // correctly falls back to the simpler result_ok assertion (is_ok()) because
        // a real assertion is better than a TODO stub. (#818)
        assert!(
            rendered.contains("result.is_ok()"),
            "without type registry, should fall back to is_ok() assertion, got:\n{}",
            rendered
        );

        // Should mention the branch condition in the assertion message
        assert!(
            rendered.contains("changed_files.is_empty()"),
            "should reference the branch condition, got:\n{}",
            rendered
        );
    }

    #[test]
    fn test_full_pipeline_renders_field_assertions_with_type_registry() {
        let contract = sample_result_contract();
        let grammar = full_grammar();

        // Build a type registry containing ValidationResult
        let mut type_registry = HashMap::new();
        type_registry.insert(
            "ValidationResult".to_string(),
            TypeDefinition {
                name: "ValidationResult".to_string(),
                kind: "struct".to_string(),
                file: "src/core/engine/validate_write.rs".to_string(),
                line: 10,
                fields: vec![
                    FieldDef {
                        name: "success".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "command".to_string(),
                        field_type: "Option<String>".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "output".to_string(),
                        field_type: "Option<String>".to_string(),
                        is_public: true,
                    },
                    FieldDef {
                        name: "rolled_back".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                ],
                is_public: true,
            },
        );

        // Use generate_test_plan_with_types — this is the path the fixers use
        let plan = generate_test_plan_with_types(&contract, &grammar, &type_registry);
        let rendered = render_test_plan(&plan, &grammar.test_templates);

        // Should produce output
        assert!(!rendered.is_empty(), "rendered output should not be empty");

        // With type registry, enrichment should replace the TODO placeholder with
        // real field-level assertions instead of falling back to is_ok()
        assert!(
            rendered.contains("result.unwrap()"),
            "with type registry, should unwrap() to get at the inner value, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.success, false)"),
            "should assert success field, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.command, None)"),
            "should assert command field, got:\n{}",
            rendered
        );
        assert!(
            rendered.contains("assert_eq!(inner.rolled_back, false)"),
            "should assert rolled_back field, got:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("// TODO:"),
            "should NOT contain TODO placeholders, got:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("is_ok()"),
            "should NOT fall back to is_ok() when type info is available, got:\n{}",
            rendered
        );
    }

    #[test]
    fn test_parse_fields_from_rust_struct_source() {
        let source = r#"
pub struct ValidationResult {
    pub success: bool,
    pub command: Option<String>,
    pub output: Option<String>,
    pub rolled_back: bool,
    files_checked: usize,
}
"#;
        let fields = parse_fields_from_source(
            source,
            r"^\s*(?:pub\s+)?(\w+)\s*:\s*(.+?),?\s*$",
            Some(r"^\s*pub\b"),
            1, // name_group
            2, // type_group
        );

        assert_eq!(fields.len(), 5, "should find 5 fields");
        assert_eq!(fields[0].name, "success");
        assert_eq!(fields[0].field_type, "bool");
        assert!(fields[0].is_public, "success should be public");
        assert_eq!(fields[1].name, "command");
        assert_eq!(fields[1].field_type, "Option<String>");
        assert_eq!(fields[4].name, "files_checked");
        assert!(!fields[4].is_public, "files_checked should be private");
    }

    #[test]
    fn test_parse_fields_from_php_class_source() {
        let source = r#"
class AbilityResult {
    public string $status;
    public ?array $data;
    protected int $code;
    private string $internal_key;
    public bool $success;
}
"#;
        // PHP: type is group 1, name is group 2
        let fields = parse_fields_from_source(
            source,
            r"^\s*(?:public|protected|private)\s+(?:readonly\s+)?(\??\w+)\s+\$(\w+)",
            Some(r"^\s*public\b"),
            2, // name_group (PHP has name in group 2)
            1, // type_group (PHP has type in group 1)
        );

        assert_eq!(
            fields.len(),
            5,
            "should find 5 PHP properties, got {:?}",
            fields
        );
        assert_eq!(fields[0].name, "status");
        assert_eq!(fields[0].field_type, "string");
        assert!(fields[0].is_public, "status should be public");
        assert_eq!(fields[1].name, "data");
        assert_eq!(fields[1].field_type, "?array");
        assert_eq!(fields[2].name, "code");
        assert_eq!(fields[2].field_type, "int");
        assert!(
            !fields[2].is_public,
            "code should not be public (protected)"
        );
        assert_eq!(fields[4].name, "success");
        assert!(fields[4].is_public, "success should be public");
    }


    #[test]
    fn test_generate_test_plan_default_path() {

        let _result = generate_test_plan();
    }

    #[test]
    fn test_generate_test_plan_with_types_if_let_some_hint_infer_hint_for_param_b_condition_cond_lower() {

        let _result = generate_test_plan_with_types();
    }

    #[test]
    fn test_generate_test_plan_with_types_some_branch() {

        let _result = generate_test_plan_with_types();
    }

    #[test]
    fn test_generate_test_plan_with_types_if_let_some_ref_so_setup_override() {

        let _result = generate_test_plan_with_types();
    }

    #[test]
    fn test_generate_test_plan_with_types_expected_value_some_effect_names_join() {

        let _result = generate_test_plan_with_types();
    }

    #[test]
    fn test_generate_test_plan_with_types_has_expected_effects() {
        // Expected effects: mutation

        let _ = generate_test_plan_with_types();
    }

    #[test]
    fn test_render_test_plan_some_t_t() {

        let _result = render_test_plan();
    }

    #[test]
    fn test_render_test_plan_match_templates_get_default() {

        let _result = render_test_plan();
    }

    #[test]
    fn test_render_test_plan_has_expected_effects() {
        // Expected effects: mutation

        let _ = render_test_plan();
    }

    #[test]
    fn test_build_project_type_registry_some_p_p_clone() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: Some(p) => p.clone(),");
    }

    #[test]
    fn test_build_project_type_registry_ok_c_c() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: Ok(c) => c,");
    }

    #[test]
    fn test_build_project_type_registry_err_continue() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: Err(_) => continue,");
    }

    #[test]
    fn test_build_project_type_registry_some_g_g() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: Some(g) => g,");
    }

    #[test]
    fn test_build_project_type_registry_sym_concept_struct_sym_concept_class() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: sym.concept != \"struct\" && sym.concept != \"class\"");
    }

    #[test]
    fn test_build_project_type_registry_some_s_s() {
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let result = build_project_type_registry(&root, &_grammar, &contract_grammar);
        assert!(!result.is_empty(), "expected non-empty collection for: Some(s) => s,");
    }

    #[test]
    fn test_build_project_type_registry_has_expected_effects() {
        // Expected effects: mutation, file_read, logging
        let root = Default::default();
        let _grammar = Default::default();
        let contract_grammar = Default::default();
        let _ = build_project_type_registry(&root, &_grammar, &contract_grammar);
    }

    #[test]
    fn test_generate_tests_for_file_default_path() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let _result = generate_tests_for_file(&content, &file_path, &grammar);
    }

    #[test]
    fn test_generate_tests_for_file_with_types_default_path() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let _result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
    }

    #[test]
    fn test_generate_tests_for_file_with_types_contract_grammar_test_templates_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
        assert!(result.is_none(), "expected None for: contract_grammar.test_templates.is_empty()");
    }

    #[test]
    fn test_generate_tests_for_file_with_types_default_path_2() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let _result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
    }

    #[test]
    fn test_generate_tests_for_file_with_types_contracts_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
        assert!(result.is_none(), "expected None for: contracts.is_empty()");
    }

    #[test]
    fn test_generate_tests_for_file_with_types_if_let_some_project_reg_project_type_registry() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = Some(Default::default());
        let result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
        assert!(result.is_some(), "expected Some for: if let Some(project_reg) = project_type_registry {{");
    }

    #[test]
    fn test_generate_tests_for_file_with_types_if_let_some_imports_str_case_variables_get_extra_imports() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
        assert!(result.is_some(), "expected Some for: if let Some(imports_str) = case.variables.get(\"extra_imports\") {{");
    }

    #[test]
    fn test_generate_tests_for_file_with_types_test_source_trim_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let result = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
        assert!(result.is_none(), "expected None for: test_source.trim().is_empty()");
    }

    #[test]
    fn test_generate_tests_for_file_with_types_has_expected_effects() {
        // Expected effects: mutation
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let project_type_registry = None;
        let _ = generate_tests_for_file_with_types(&content, &file_path, &grammar, project_type_registry);
    }

    #[test]
    fn test_generate_tests_for_methods_default_path() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let _result = generate_tests_for_methods(&content, &file_path, &grammar, &method_names);
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_default_path() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let _result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_contract_grammar_test_templates_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
        assert!(result.is_none(), "expected None for: contract_grammar.test_templates.is_empty()");
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_contract_grammar_test_templates_is_empty_2() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let _result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_contracts_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
        assert!(result.is_none(), "expected None for: contracts.is_empty()");
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_if_let_some_project_reg_project_type_registry() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = Some(Default::default());
        let result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
        assert!(result.is_some(), "expected Some for: if let Some(project_reg) = project_type_registry {{");
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_plan_cases_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
        assert!(result.is_some(), "expected Some for: plan.cases.is_empty()");
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_test_source_trim_is_empty() {
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let result = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
        assert!(result.is_none(), "expected None for: test_source.trim().is_empty()");
    }

    #[test]
    fn test_generate_tests_for_methods_with_types_has_expected_effects() {
        // Expected effects: mutation
        let content = "";
        let file_path = "";
        let grammar = Default::default();
        let method_names = Vec::new();
        let project_type_registry = None;
        let _ = generate_tests_for_methods_with_types(&content, &file_path, &grammar, &method_names, project_type_registry);
    }

}
