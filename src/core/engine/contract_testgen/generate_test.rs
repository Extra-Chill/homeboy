//! generate_test — extracted from contract_testgen.rs.

use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use regex::Regex;
use serde::{Deserialize, Serialize};
use super::slugify;
use super::infer_setup_with_complements;
use super::resolve_assertion;
use super::derive_template_key;
use super::sanitize_for_string_literal;
use super::TestPlan;
use super::fallback_to_simple_assertion;
use super::infer_hint_for_param;
use super::build_variables;
use super::TestCase;
use super::merge_imports;
use super::super::contract::*;


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
pub fn generate_test_plan(
    contract: &FunctionContract,
    contract_grammar: &ContractGrammar,
) -> TestPlan {
    generate_test_plan_with_types(contract, contract_grammar, &HashMap::new())
}

/// Generate a test plan with access to a type registry for struct introspection.
pub fn generate_test_plan_with_types(
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
                    if let Some(hint) = infer_hint_for_param(&b.condition, &cond_lower, param) {
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
            vars.insert(
                "condition".to_string(),
                sanitize_for_string_literal(&branch.condition),
            );
            vars.insert("condition_slug".to_string(), slugify(&branch.condition));

            // Behavioral inference: derive setup overrides from branch condition,
            // with cross-branch complement hints for unmatched params.
            let complement_hints = build_complement_hints(i, &all_branch_hints);
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
                contract_grammar.field_assertion_template.as_deref(),
            );

            // If the assertion still has a TODO placeholder after enrichment,
            // fall back to the simpler non-value assertion (e.g. result_ok instead
            // of result_ok_value). A test that asserts is_ok() is better than a
            // stub with `let _ = inner; // TODO: assert ...`. (#818)
            let assertion = if assertion.contains("// TODO:") {
                fallback_to_simple_assertion(
                    &branch.returns,
                    &contract.signature.return_type,
                    &branch.condition,
                    &contract_grammar.assertion_templates,
                )
                .unwrap_or(assertion)
            } else {
                assertion
            };
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
        is_async: contract.signature.is_async,
        cases,
    }
}
