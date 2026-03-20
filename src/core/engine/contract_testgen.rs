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

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::contract::*;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};

/// A plan for generating tests for a single function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    /// The function being tested.
    pub function_name: String,
    /// Source file containing the function.
    pub source_file: String,
    /// Individual test cases to generate.
    pub cases: Vec<TestCase>,
}

/// A single test case to generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Suggested test function name (e.g., "test_validate_write_skips_when_empty").
    pub test_name: String,
    /// Which branch this test covers.
    pub branch_condition: String,
    /// The expected return variant (ok, err, some, none, true, false, value).
    pub expected_variant: String,
    /// Description of what the expected return value should be.
    pub expected_value: Option<String>,
    /// The template key to use for rendering (e.g., "result_ok", "option_none", "bool_true").
    pub template_key: String,
    /// Template variables for rendering.
    pub variables: HashMap<String, String>,
}

/// Generate a test plan from a function contract.
///
/// Produces one test case per branch with behavioral setup and assertions.
/// The plan is language-agnostic — rendering to source code requires
/// templates from the grammar.
///
/// The `contract_grammar` provides all language-specific knowledge:
/// - `type_defaults` — zero/default values for parameter types
/// - `type_constructors` — behavioral constructors for condition-specific inputs
/// - `assertion_templates` — language-specific assertion code patterns
/// - `fallback_default` — fallback expression when nothing else matches
/// - `field_pattern` — regex for extracting struct fields from source
///
/// The `type_registry` maps type names to their definitions, enabling
/// field-level assertions when the return type is a known struct.
///
/// Core analyzes conditions and returns to produce **semantic hints**, then
/// resolves those hints through the grammar to get language-specific code.
/// Convenience wrapper — generates a test plan without a type registry.
#[cfg(test)]
pub(crate) fn generate_test_plan(
    contract: &FunctionContract,
    contract_grammar: &ContractGrammar,
) -> TestPlan {
    generate_test_plan_with_types(contract, contract_grammar, &HashMap::new())
}

/// Generate a test plan with access to a type registry for struct introspection.
pub(crate) fn generate_test_plan_with_types(
    contract: &FunctionContract,
    contract_grammar: &ContractGrammar,
    type_registry: &HashMap<String, TypeDefinition>,
) -> TestPlan {
    let mut cases = Vec::new();
    let type_defaults = &contract_grammar.type_defaults;

    if contract.branches.is_empty() {
        // No branches detected — generate a basic "does not panic" test
        cases.push(TestCase {
            test_name: format!("test_{}_does_not_panic", contract.name),
            branch_condition: "default invocation".to_string(),
            expected_variant: "no_panic".to_string(),
            expected_value: None,
            template_key: "no_panic".to_string(),
            variables: build_variables(
                contract,
                None,
                type_defaults,
                &contract_grammar.fallback_default,
            ),
        });
    } else {
        // First pass: collect hints from all branches so we can infer complements.
        // If branch 1 says param X should be "empty", branches that don't mention X
        // should use "non_empty" to reach a different code path.
        let all_branch_hints: Vec<HashMap<String, String>> = contract
            .branches
            .iter()
            .map(|b| {
                let cond_lower = b.condition.to_lowercase();
                let mut hints_for_branch = HashMap::new();
                for param in &contract.signature.params {
                    if let Some(hint) =
                        infer_hint_for_param(&b.condition, &cond_lower, param)
                    {
                        hints_for_branch.insert(param.name.clone(), hint);
                    }
                }
                hints_for_branch
            })
            .collect();

        for (i, branch) in contract.branches.iter().enumerate() {
            let condition_slug = slugify(&branch.condition);
            let test_name = format!(
                "test_{}_{}",
                contract.name,
                if condition_slug.is_empty() {
                    format!("branch_{}", i)
                } else {
                    condition_slug
                }
            );

            let template_key =
                derive_template_key(&contract.signature.return_type, &branch.returns);

            let mut vars = build_variables(
                contract,
                Some(branch),
                type_defaults,
                &contract_grammar.fallback_default,
            );
            vars.insert("condition".to_string(), branch.condition.clone());
            vars.insert("condition_slug".to_string(), slugify(&branch.condition));

            // Behavioral inference: derive setup overrides from branch condition,
            // with cross-branch complement hints for unmatched params.
            let complement_hints =
                build_complement_hints(i, &all_branch_hints);
            let setup_override = infer_setup_with_complements(
                &branch.condition,
                &contract.signature.params,
                type_defaults,
                &contract_grammar.type_constructors,
                &contract_grammar.fallback_default,
                &complement_hints,
            );
            if let Some(ref so) = setup_override {
                vars.insert("param_setup".to_string(), so.setup_lines.clone());
                vars.insert("param_args".to_string(), so.call_args.clone());
                if !so.extra_imports.is_empty() {
                    let existing = vars.get("extra_imports").cloned().unwrap_or_default();
                    let merged = merge_imports(&existing, &so.extra_imports);
                    vars.insert("extra_imports".to_string(), merged);
                }
            }

            // Behavioral inference: derive assertion from branch return.
            // Core selects an assertion key; grammar provides the template.
            // When a type registry is available, assertions can reference
            // specific struct fields instead of using opaque TODO placeholders.
            let assertion = resolve_assertion(
                &branch.returns,
                &contract.signature.return_type,
                &branch.condition,
                &contract_grammar.assertion_templates,
            );

            // If we have type info and the assertion has a TODO placeholder,
            // replace it with real field-level assertions using type defaults.
            let assertion = enrich_assertion_with_fields(
                &assertion,
                &branch.returns,
                &contract.signature.return_type,
                type_registry,
                type_defaults,
                &contract_grammar.fallback_default,
            );
            vars.insert("assertion_code".to_string(), assertion);

            cases.push(TestCase {
                test_name,
                branch_condition: branch.condition.clone(),
                expected_variant: branch.returns.variant.clone(),
                expected_value: branch.returns.value.clone(),
                template_key,
                variables: vars,
            });
        }
    }

    // If the function has effects, generate an effect-specific test
    if contract.has_effects() && !contract.effects.is_empty() {
        let effect_names: Vec<&str> = contract
            .effects
            .iter()
            .map(|e| match e {
                Effect::FileRead => "file_read",
                Effect::FileWrite => "file_write",
                Effect::FileDelete => "file_delete",
                Effect::ProcessSpawn { .. } => "process_spawn",
                Effect::Mutation { .. } => "mutation",
                Effect::Panic { .. } => "panic",
                Effect::Network => "network",
                Effect::ResourceAlloc { .. } => "resource_alloc",
                Effect::Logging => "logging",
            })
            .collect();

        let mut vars = build_variables(
            contract,
            None,
            type_defaults,
            &contract_grammar.fallback_default,
        );
        vars.insert("effects".to_string(), effect_names.join(", "));

        cases.push(TestCase {
            test_name: format!("test_{}_has_expected_effects", contract.name),
            branch_condition: "effect verification".to_string(),
            expected_variant: "effects".to_string(),
            expected_value: Some(effect_names.join(", ")),
            template_key: "effects".to_string(),
            variables: vars,
        });
    }

    TestPlan {
        function_name: contract.name.clone(),
        source_file: contract.file.clone(),
        cases,
    }
}

/// Render a test plan into source code using templates.
///
/// Templates are key → string pairs where keys match `TestCase.template_key`.
/// Template variables are replaced: `{fn_name}`, `{fn_call}`, `{param_list}`, etc.
pub(crate) fn render_test_plan(plan: &TestPlan, templates: &HashMap<String, String>) -> String {
    let mut output = String::new();

    for case in &plan.cases {
        let template = match templates.get(&case.template_key) {
            Some(t) => t,
            None => {
                // Fall back to a generic template if the specific one doesn't exist
                match templates.get("default") {
                    Some(t) => t,
                    None => continue,
                }
            }
        };

        let mut rendered = template.clone();
        for (key, value) in &case.variables {
            rendered = rendered.replace(&format!("{{{}}}", key), value);
        }
        // Also replace the test name
        rendered = rendered.replace("{test_name}", &case.test_name);

        output.push_str(&rendered);
        output.push('\n');
    }

    output
}

/// Build template variables from a contract and optional branch.
fn build_variables(
    contract: &FunctionContract,
    branch: Option<&Branch>,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    vars.insert("fn_name".to_string(), contract.name.clone());
    vars.insert("file".to_string(), contract.file.clone());
    vars.insert("line".to_string(), contract.line.to_string());

    // Build param list for function call
    let param_names: Vec<&str> = contract
        .signature
        .params
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    vars.insert("param_names".to_string(), param_names.join(", "));

    // Build typed param declarations
    let param_decls: Vec<String> = contract
        .signature
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.param_type))
        .collect();
    vars.insert("param_decls".to_string(), param_decls.join(", "));

    // Param count
    vars.insert(
        "param_count".to_string(),
        contract.signature.params.len().to_string(),
    );

    // Build param setup lines and call args using type_defaults
    let (setup_lines, call_args, extra_imports) =
        build_param_inputs(&contract.signature.params, type_defaults, fallback_default);
    vars.insert("param_setup".to_string(), setup_lines);
    vars.insert("param_args".to_string(), call_args);
    vars.insert("extra_imports".to_string(), extra_imports);

    // Return type info
    match &contract.signature.return_type {
        ReturnShape::Unit => vars.insert("return_shape".to_string(), "unit".to_string()),
        ReturnShape::Bool => vars.insert("return_shape".to_string(), "bool".to_string()),
        ReturnShape::Value { value_type } => {
            vars.insert("return_shape".to_string(), "value".to_string());
            vars.insert("return_type".to_string(), value_type.clone())
        }
        ReturnShape::OptionType { some_type } => {
            vars.insert("return_shape".to_string(), "option".to_string());
            vars.insert("some_type".to_string(), some_type.clone())
        }
        ReturnShape::ResultType { ok_type, err_type } => {
            vars.insert("return_shape".to_string(), "result".to_string());
            vars.insert("ok_type".to_string(), ok_type.clone());
            vars.insert("err_type".to_string(), err_type.clone())
        }
        ReturnShape::Collection { element_type } => {
            vars.insert("return_shape".to_string(), "collection".to_string());
            vars.insert("element_type".to_string(), element_type.clone())
        }
        ReturnShape::Unknown { raw } => {
            vars.insert("return_shape".to_string(), "unknown".to_string());
            vars.insert("return_type".to_string(), raw.clone())
        }
    };

    // Branch-specific variables
    if let Some(branch) = branch {
        vars.insert("variant".to_string(), branch.returns.variant.clone());
        if let Some(ref val) = branch.returns.value {
            vars.insert("expected_value".to_string(), val.clone());
        }
    }

    // Is it a method (has receiver)?
    vars.insert(
        "is_method".to_string(),
        contract.signature.receiver.is_some().to_string(),
    );
    vars.insert("is_pure".to_string(), contract.is_pure().to_string());
    vars.insert(
        "branch_count".to_string(),
        contract.branch_count().to_string(),
    );

    vars
}

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

/// Build parameter setup lines, call arguments, and extra imports from type_defaults.
///
/// Returns `(setup_lines, call_args, extra_imports)` where:
/// - `setup_lines` is newline-separated `let` bindings
/// - `call_args` is comma-separated arguments for the function call
/// - `extra_imports` is newline-separated `use` statements
fn build_param_inputs(
    params: &[Param],
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> (String, String, String) {
    if params.is_empty() {
        return (String::new(), String::new(), String::new());
    }

    let mut setup_lines = Vec::new();
    let mut call_args = Vec::new();
    let mut all_imports: Vec<String> = Vec::new();

    for param in params {
        let (value_expr, call_override, imports) =
            resolve_type_default(&param.param_type, type_defaults, fallback_default);

        // Build the let binding
        setup_lines.push(format!("        let {} = {};", param.name, value_expr));

        // Build the call argument — if the type is a reference, borrow the variable
        let call_arg = call_override.unwrap_or_else(|| {
            let trimmed = param.param_type.trim();
            if trimmed.starts_with('&') {
                format!("&{}", param.name)
            } else {
                param.name.clone()
            }
        });
        call_args.push(call_arg);

        for imp in imports {
            let imp_string = imp.to_string();
            if !all_imports.contains(&imp_string) {
                all_imports.push(imp_string);
            }
        }
    }

    let setup = setup_lines.join("\n");
    let args = call_args.join(", ");
    let imports = all_imports.join("\n");
    (setup, args, imports)
}

/// Derive the template key from the return type shape and the branch's return variant.
fn derive_template_key(return_type: &ReturnShape, returns: &ReturnValue) -> String {
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
fn slugify(s: &str) -> String {
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

/// Overridden setup derived from a branch condition.
struct SetupOverride {
    /// Newline-separated `let` bindings (8-space indented).
    setup_lines: String,
    /// Comma-separated call arguments.
    call_args: String,
    /// Extra `use` imports needed.
    extra_imports: String,
}

/// Infer parameter setup code from a branch condition string (without cross-branch complements).
///
/// Delegates to `infer_setup_with_complements` with no complement hints.
/// Used in tests and simple single-branch scenarios.
#[cfg(test)]
fn infer_setup_from_condition(
    condition: &str,
    params: &[Param],
    type_defaults: &[TypeDefault],
    type_constructors: &[TypeConstructor],
    fallback_default: &str,
) -> Option<SetupOverride> {
    let condition_lower = condition.to_lowercase();

    // Step 1: Produce semantic hints for each parameter
    let mut param_hints: HashMap<String, String> = HashMap::new();
    for param in params {
        if let Some(hint) = infer_hint_for_param(condition, &condition_lower, param) {
            param_hints.insert(param.name.clone(), hint);
        }
    }

    if param_hints.is_empty() {
        return None;
    }

    // Step 2: Resolve hints through grammar constructors
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
            // No hint for this param — use type_defaults
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

/// Produce the default call argument for a parameter based on its type.
fn default_call_arg(name: &str, param_type: &str) -> String {
    if param_type.trim().starts_with('&') {
        format!("&{}", name)
    } else {
        name.to_string()
    }
}

/// Analyze a branch condition to produce a semantic hint for a parameter.
///
/// This is the core of behavioral inference — it recognizes common condition
/// patterns and maps them to language-agnostic hints. The hints are then
/// resolved through the grammar's `type_constructors` to get actual code.
///
/// Returns `None` if no hint can be inferred for this parameter.
fn infer_hint_for_param(condition: &str, condition_lower: &str, param: &Param) -> Option<String> {
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

/// Resolve a semantic hint + param type through the grammar's type_constructors.
///
/// Tries constructors in order; first match on both `hint` and `pattern` wins.
/// Falls back to `type_defaults` if no constructor matches, then to `fallback_default`.
///
/// The `{param_name}` placeholder in constructor values is replaced with the
/// actual parameter name.
fn resolve_constructor(
    hint: &str,
    param_name: &str,
    param_type: &str,
    constructors: &[TypeConstructor],
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> (String, String, Vec<String>) {
    // Split compound hints like "contains:foo" into base hint + argument
    let (base_hint, hint_arg) = if let Some(colon_pos) = hint.find(':') {
        (&hint[..colon_pos], Some(&hint[colon_pos + 1..]))
    } else {
        (hint, None)
    };

    // Try type_constructors first
    for tc in constructors {
        if tc.hint != base_hint {
            continue;
        }
        if let Ok(re) = Regex::new(&tc.pattern) {
            if re.is_match(param_type) {
                // Found a match — apply parameter name substitution
                let mut value = tc.value.replace("{param_name}", param_name);
                // For "contains" hints, also substitute the literal argument
                if let Some(arg) = hint_arg {
                    value = value.replace("{hint_arg}", arg);
                }

                let call_arg = tc
                    .call_arg
                    .as_ref()
                    .map(|c| c.replace("{param_name}", param_name))
                    .unwrap_or_else(|| default_call_arg(param_name, param_type));

                let imports: Vec<String> = tc.imports.to_vec();
                return (value, call_arg, imports);
            }
        }
    }

    // No constructor matched — fall back to type_defaults
    let (val, call_override, imps) =
        resolve_type_default(param_type, type_defaults, fallback_default);
    let call = call_override.unwrap_or_else(|| default_call_arg(param_name, param_type));
    let imp_strs: Vec<String> = imps.into_iter().map(|s| s.to_string()).collect();
    (val, call, imp_strs)
}

/// Check if condition contains `param.method()` (case-insensitive).
fn condition_contains_param_method(condition_lower: &str, param: &str, method: &str) -> bool {
    let pattern = format!("{}.{}(", param.to_lowercase(), method);
    condition_lower.contains(&pattern)
}

/// Check if condition contains a negated method call: `!param.method()`.
fn condition_contains_negated_method(condition: &str, param: &str, method: &str) -> bool {
    let pattern = format!("!{}.{}(", param, method);
    condition.contains(&pattern)
}

/// Check if a type looks like a filesystem path (language-agnostic heuristic).
fn is_path_like(ptype: &str) -> bool {
    let t = ptype.trim().to_lowercase();
    t.contains("path")
}

/// Check if a type looks like a numeric type (language-agnostic heuristic).
fn is_numeric_like(ptype: &str) -> bool {
    let t = ptype.trim();
    // Common numeric type patterns across languages
    matches!(
        t,
        "usize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "isize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "int"
            | "float"
            | "double"
            | "number"
    )
}

/// Extract a string literal argument from a method call in a condition.
///
/// E.g., from `name.contains("foo")` extracts `"foo"`.
fn extract_method_string_arg(condition: &str, param: &str, method: &str) -> Option<String> {
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
        // Substitute variables in the assertion template
        let mut rendered = tmpl.clone();
        rendered = rendered.replace("{condition}", condition);
        if let Some(ref val) = returns.value {
            rendered = rendered.replace("{expected_value}", val);
        }
        rendered = rendered.replace("{variant}", variant);
        rendered
    } else {
        // No grammar template — produce a minimal language-agnostic placeholder
        format!("{indent}let _ = result; // {variant}: {condition}")
    }
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

    let type_def = match type_registry.get(base_name) {
        Some(td) => td,
        None => return assertion.to_string(),
    };

    let public_fields: Vec<&FieldDef> = type_def.fields.iter().filter(|f| f.is_public).collect();
    if public_fields.is_empty() {
        return assertion.to_string();
    }

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
        assertion[..todo_pos].rfind('\n').map(|p| p + 1).unwrap_or(0)
    };

    let replace_end = assertion[todo_pos..]
        .find('\n')
        .map(|p| todo_pos + p + 1)
        .unwrap_or(assertion.len());

    // Build real field assertions
    let mut field_assertions = Vec::new();
    for field in &public_fields {
        let expected = default_for_field_type(&field.field_type, type_defaults, fallback_default);
        field_assertions.push(format!(
            "{indent}assert_eq!(inner.{}, {});",
            field.name, expected
        ));
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
/// Uses the grammar's type_defaults first, then falls back to common patterns.
fn default_for_field_type(
    field_type: &str,
    type_defaults: &[TypeDefault],
    fallback_default: &str,
) -> String {
    let trimmed = field_type.trim();

    // Try grammar type_defaults first
    for td in type_defaults {
        if let Ok(re) = Regex::new(&td.pattern) {
            if re.is_match(trimmed) {
                return td.value.clone();
            }
        }
    }

    // Common fallbacks for types that might not be in type_defaults
    if trimmed == "bool" {
        return "false".to_string();
    }
    if trimmed == "usize" || trimmed == "u32" || trimmed == "u64" || trimmed == "i32" || trimmed == "i64" {
        return "0".to_string();
    }
    if trimmed.starts_with("Option<") || trimmed.starts_with("Option ") {
        return "None".to_string();
    }
    if trimmed.starts_with("Vec<") {
        return "vec![]".to_string();
    }
    if trimmed == "String" {
        return "String::new()".to_string();
    }
    if trimmed == "&str" {
        return r#""""#.to_string();
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
        let (value_expr, call_arg, imports) =
            if let Some(hint) = param_hints.get(&param.name) {
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
/// When the grammar has `field_pattern`, struct definitions in the file are
/// parsed into a type registry, enabling field-level assertions in generated tests.
pub fn generate_tests_for_file(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
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

    // Build type registry from struct definitions in this file
    let type_registry = build_type_registry(content, file_path, grammar, contract_grammar);

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

        let plan = generate_test_plan_with_types(contract, contract_grammar, &type_registry);
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
pub fn generate_tests_for_methods(
    content: &str,
    file_path: &str,
    grammar: &crate::extension::grammar::Grammar,
    method_names: &[&str],
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

    let type_registry = build_type_registry(content, file_path, grammar, contract_grammar);

    let mut test_source = String::new();
    let mut all_extra_imports: Vec<String> = Vec::new();
    let mut tested_functions = Vec::new();

    for contract in &contracts {
        // Only generate tests for the requested methods
        if !method_names.contains(&contract.name.as_str()) {
            continue;
        }

        let plan = generate_test_plan_with_types(contract, contract_grammar, &type_registry);
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
        ]
    }

    /// Build a minimal ContractGrammar for testing.
    fn empty_grammar() -> ContractGrammar {
        ContractGrammar::default()
    }

    /// Build a ContractGrammar with type_defaults populated.
    fn grammar_with_defaults() -> ContractGrammar {
        ContractGrammar {
            type_defaults: sample_type_defaults(),
            ..Default::default()
        }
    }

    /// Build a ContractGrammar with type_defaults + type_constructors + assertion_templates + test_templates.
    fn full_grammar() -> ContractGrammar {
        ContractGrammar {
            type_defaults: sample_type_defaults(),
            type_constructors: sample_type_constructors(),
            assertion_templates: sample_assertion_templates(),
            test_templates: sample_test_templates(),
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

        // First branch (changed_files.is_empty()) should unwrap the Ok value
        assert!(
            rendered.contains("result.unwrap()"),
            "should unwrap Ok value instead of just is_ok(), got:\n{}",
            rendered
        );
        // Should mention the expected value from the contract
        assert!(
            rendered.contains("skipped"),
            "should reference the expected return value 'skipped', got:\n{}",
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
    fn test_enrich_assertion_replaces_todo_with_field_hints() {
        let mut type_registry = HashMap::new();
        type_registry.insert(
            "ValidationResult".to_string(),
            TypeDefinition {
                name: "ValidationResult".to_string(),
                kind: "struct".to_string(),
                file: "src/test.rs".to_string(),
                line: 1,
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
                        name: "rolled_back".to_string(),
                        field_type: "bool".to_string(),
                        is_public: true,
                    },
                ],
                is_public: true,
            },
        );

        let assertion = "        let inner = result.unwrap();\n        // Branch returns Ok(skipped)\n        let _ = inner; // TODO: assert specific value for \"skipped\"";
        let return_type = ReturnShape::ResultType {
            ok_type: "ValidationResult".to_string(),
            err_type: "Error".to_string(),
        };
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("skipped".to_string()),
        };

        let type_defaults = sample_type_defaults();
        let enriched = enrich_assertion_with_fields(
            assertion,
            &returns,
            &return_type,
            &type_registry,
            &type_defaults,
            "Default::default()",
        );

        // Should generate real assert_eq! statements, not comments
        assert!(
            enriched.contains("assert_eq!(inner.success, false)"),
            "should assert success field equals false, got:\n{}",
            enriched
        );
        assert!(
            enriched.contains("assert_eq!(inner.command, None)"),
            "should assert command field equals None, got:\n{}",
            enriched
        );
        assert!(
            enriched.contains("assert_eq!(inner.rolled_back, false)"),
            "should assert rolled_back field equals false, got:\n{}",
            enriched
        );
        assert!(
            !enriched.contains("TODO:"),
            "should replace the TODO placeholder, got:\n{}",
            enriched
        );
        assert!(
            !enriched.contains("let _ = inner"),
            "should remove the let _ = inner placeholder, got:\n{}",
            enriched
        );
    }

    #[test]
    fn test_enrich_assertion_skips_when_no_type_in_registry() {
        let type_registry = HashMap::new();

        let assertion = "        let _ = inner; // TODO: assert something";
        let return_type = ReturnShape::ResultType {
            ok_type: "UnknownType".to_string(),
            err_type: "Error".to_string(),
        };
        let returns = ReturnValue {
            variant: "ok".to_string(),
            value: Some("val".to_string()),
        };

        let enriched = enrich_assertion_with_fields(
            assertion,
            &returns,
            &return_type,
            &type_registry,
            &[],
            "Default::default()",
        );

        assert_eq!(
            enriched, assertion,
            "should return assertion unchanged when type is not in registry"
        );
    }
}
