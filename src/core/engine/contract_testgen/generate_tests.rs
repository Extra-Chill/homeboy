//! generate_tests — extracted from contract_testgen.rs.

use std::collections::HashMap;
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};
use super::GeneratedTestOutput;
use super::super::contract::*;
use super::super::*;


/// Build a type registry from struct/class definitions found in a source file.
///
/// Uses the grammar's symbol extraction to find struct/enum/class definitions,
/// then parses their field declarations using the grammar's `field_pattern`.
/// Returns a map from type name to `TypeDefinition`.
pub(crate) fn build_type_registry(
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
