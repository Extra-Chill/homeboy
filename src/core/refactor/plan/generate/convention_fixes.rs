use crate::code_audit::conventions::Language;
use crate::code_audit::naming::{detect_naming_suffix, suffix_matches};
use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{Fix, InsertionKind, SkippedFile};

use regex::Regex;
use std::collections::HashMap;
use std::path::Path;

use super::signatures::MethodSignature;
use super::{
    extract_signatures_from_items, generate_fallback_signature, generate_method_stub, insertion,
    primary_type_name_from_declaration,
};

pub(crate) fn generate_import_statement(import_path: &str, language: &Language) -> String {
    match language {
        Language::Rust => format!("use {};", import_path),
        Language::Php => format!("use {};", import_path),
        Language::JavaScript | Language::TypeScript => {
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

pub(crate) fn generate_namespace_declaration(
    namespace: &str,
    language: &Language,
) -> Option<String> {
    match language {
        Language::Php => Some(format!("namespace {};", namespace)),
        _ => None,
    }
}

pub(crate) fn generate_type_conformance_declaration(
    type_name: &str,
    conformance: &str,
    language: &Language,
) -> String {
    match language {
        Language::Rust => format!("\nimpl {} for {} {{\n}}\n", conformance, type_name),
        Language::Php | Language::TypeScript => conformance.to_string(),
        Language::JavaScript | Language::Unknown => conformance.to_string(),
    }
}

pub(crate) fn generate_registration_stub(hook_name: &str) -> String {
    let callback = hook_name
        .strip_prefix("wp_")
        .or_else(|| hook_name.strip_prefix("datamachine_"))
        .unwrap_or(hook_name);

    format!(
        "        add_action('{}', [$this, '{}']);",
        hook_name, callback
    )
}

pub(crate) fn build_signature_map(
    conforming_files: &[String],
    root: &Path,
) -> HashMap<String, MethodSignature> {
    let mut sig_map = HashMap::new();

    for rel_path in conforming_files {
        let abs_path = root.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&abs_path) {
            let language = Language::from_path(&abs_path);
            for sig in extract_signatures_from_items(&content, &language) {
                sig_map.entry(sig.name.clone()).or_insert(sig);
            }
        }
    }

    sig_map
}

pub(crate) fn file_has_constructor(content: &str, language: &Language) -> bool {
    match language {
        Language::Php => content.contains("function __construct"),
        Language::Rust => content.contains("fn new("),
        Language::JavaScript | Language::TypeScript => content.contains("constructor("),
        Language::Unknown => false,
    }
}

fn extract_expected_namespace(description: &str) -> Option<String> {
    let expected_re = Regex::new(r"expected `([^`]+)`").ok()?;
    expected_re
        .captures(description)
        .map(|cap| cap[1].to_string())
}

pub(super) fn apply_convention_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    for conv_report in &result.conventions {
        if conv_report.outliers.is_empty() {
            continue;
        }

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

        let conforming_names: Vec<String> = conv_report
            .conforming
            .iter()
            .filter_map(|file| {
                Path::new(file)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
            })
            .collect();
        let naming_suffix = detect_naming_suffix(&conforming_names);
        let sig_map = build_signature_map(&conv_report.conforming, root);

        for outlier in &conv_report.outliers {
            if let Some(ref suffix) = naming_suffix {
                let file_stem = Path::new(&outlier.file)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
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
            let language = Language::from_path(&abs_path);
            let content = std::fs::read_to_string(&abs_path).unwrap_or_default();
            let has_constructor = file_has_constructor(&content, &language);

            let mut missing_methods: Vec<&str> = Vec::new();
            let mut missing_registrations: Vec<&str> = Vec::new();
            let mut missing_imports: Vec<&str> = Vec::new();
            let mut missing_interfaces: Vec<&str> = Vec::new();
            let mut namespace_declarations: Vec<String> = Vec::new();
            let mut needs_constructor = false;

            for deviation in &outlier.deviations {
                match &deviation.kind {
                    AuditFinding::MissingMethod => {
                        let method_name = deviation
                            .description
                            .strip_prefix("Missing method: ")
                            .unwrap_or(&deviation.description);

                        if method_name.len() < 3 {
                            continue;
                        }

                        if matches!(method_name, "__construct" | "new" | "constructor") {
                            needs_constructor = true;
                        } else {
                            missing_methods.push(method_name);
                        }
                    }
                    AuditFinding::MissingRegistration => {
                        let hook_name = deviation
                            .description
                            .strip_prefix("Missing registration: ")
                            .unwrap_or(&deviation.description);
                        missing_registrations.push(hook_name);
                    }
                    AuditFinding::MissingImport => {
                        let import_path = deviation
                            .description
                            .strip_prefix("Missing import: ")
                            .unwrap_or(&deviation.description);
                        missing_imports.push(import_path);
                    }
                    AuditFinding::MissingInterface => {
                        let conformance = deviation
                            .description
                            .strip_prefix("Missing interface: ")
                            .unwrap_or(&deviation.description);
                        missing_interfaces.push(conformance);
                    }
                    AuditFinding::NamespaceMismatch => {
                        if let Some(expected_namespace) =
                            extract_expected_namespace(&deviation.description)
                        {
                            if let Some(declaration) =
                                generate_namespace_declaration(&expected_namespace, &language)
                            {
                                namespace_declarations.push(declaration);
                            }
                        }
                    }
                    AuditFinding::DirectorySprawl => {}
                    kind if super::is_actionable_comment_finding(kind) => {}
                    _ => {}
                }
            }

            for import_path in &missing_imports {
                let use_stmt = generate_import_statement(import_path, &language);
                insertions.push(insertion(
                    InsertionKind::ImportAdd,
                    AuditFinding::MissingImport,
                    use_stmt,
                    format!("Add missing import: {}", import_path),
                ));
            }

            for conformance in &missing_interfaces {
                let Some(type_name) = content
                    .lines()
                    .find_map(|line| primary_type_name_from_declaration(line, &language))
                    .or_else(|| {
                        abs_path
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().to_string())
                    })
                else {
                    continue;
                };

                insertions.push(insertion(
                    InsertionKind::TypeConformance,
                    AuditFinding::MissingInterface,
                    generate_type_conformance_declaration(&type_name, conformance, &language),
                    format!(
                        "Add declared conformance `{}` to {}",
                        conformance, type_name
                    ),
                ));
            }

            for declaration in &namespace_declarations {
                insertions.push(insertion(
                    InsertionKind::NamespaceDeclaration,
                    AuditFinding::NamespaceMismatch,
                    declaration.clone(),
                    format!("Align namespace declaration to `{}`", declaration),
                ));
            }

            if !missing_registrations.is_empty() && language == Language::Php {
                if has_constructor && !needs_constructor {
                    for hook_name in &missing_registrations {
                        insertions.push(insertion(
                            InsertionKind::RegistrationStub,
                            AuditFinding::MissingRegistration,
                            generate_registration_stub(hook_name),
                            format!("Add {} registration in __construct()", hook_name),
                        ));
                    }
                } else {
                    let reg_lines = missing_registrations
                        .iter()
                        .map(|hook| generate_registration_stub(hook))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let construct_code = format!(
                        "\n    public function __construct() {{\n{}\n    }}\n",
                        reg_lines
                    );
                    insertions.push(insertion(
                        InsertionKind::ConstructorWithRegistration,
                        AuditFinding::MissingRegistration,
                        construct_code,
                        format!(
                            "Add __construct() with {} registration(s)",
                            missing_registrations.len()
                        ),
                    ));
                    needs_constructor = false;
                }
            }

            if needs_constructor {
                let constructor_name = match language {
                    Language::Php => "__construct",
                    Language::Rust => "new",
                    Language::JavaScript | Language::TypeScript => "constructor",
                    Language::Unknown => "__construct",
                };
                if let Some(sig) = sig_map.get(constructor_name) {
                    insertions.push(insertion(
                        InsertionKind::MethodStub,
                        AuditFinding::MissingMethod,
                        generate_method_stub(sig),
                        format!(
                            "Add {}() stub to match {} convention",
                            constructor_name, conv_report.name
                        ),
                    ));
                } else {
                    let fallback_sig = generate_fallback_signature(constructor_name, &language);
                    insertions.push(insertion(
                        InsertionKind::MethodStub,
                        AuditFinding::MissingMethod,
                        generate_method_stub(&fallback_sig),
                        format!(
                            "Add {}() stub to match {} convention (signature inferred)",
                            constructor_name, conv_report.name
                        ),
                    ));
                }
            }

            for method_name in &missing_methods {
                if let Some(sig) = sig_map.get(*method_name) {
                    insertions.push(insertion(
                        InsertionKind::MethodStub,
                        AuditFinding::MissingMethod,
                        generate_method_stub(sig),
                        format!(
                            "Add {}() stub to match {} convention",
                            method_name, conv_report.name
                        ),
                    ));
                } else {
                    let fallback_sig = generate_fallback_signature(method_name, &language);
                    insertions.push(insertion(
                        InsertionKind::MethodStub,
                        AuditFinding::MissingMethod,
                        generate_method_stub(&fallback_sig),
                        format!(
                            "Add {}() stub to match {} convention (signature inferred)",
                            method_name, conv_report.name
                        ),
                    ));
                }
            }

            if !insertions.is_empty() {
                fixes.push(Fix {
                    file: outlier.file.clone(),
                    required_methods: conv_report.expected_methods.clone(),
                    required_registrations: conv_report.expected_registrations.clone(),
                    insertions,
                    applied: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_import_statement_default_path() {

        let _result = generate_import_statement();
    }

    #[test]
    fn test_generate_namespace_declaration_match_language() {

        let result = generate_namespace_declaration();
        assert!(result.is_some(), "expected Some for: match language");
    }

    #[test]
    fn test_generate_type_conformance_declaration_default_path() {

        let _result = generate_type_conformance_declaration();
    }

    #[test]
    fn test_generate_registration_stub_default_path() {

        let _result = generate_registration_stub();
    }

    #[test]
    fn test_build_signature_map_if_let_ok_content_std_fs_read_to_string_abs_path() {

        let result = build_signature_map();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Ok(content) = std::fs::read_to_string(&abs_path) {{");
    }

    #[test]
    fn test_build_signature_map_has_expected_effects() {
        // Expected effects: file_read

        let _ = build_signature_map();
    }

    #[test]
    fn test_file_has_constructor_default_path() {

        let _result = file_has_constructor();
    }

    #[test]
    fn test_apply_convention_fixes_if_let_some_ref_suffix_naming_suffix() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_if_let_some_expected_namespace() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_let_some_expected_namespace() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_let_some_type_name_content() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_if_let_some_sig_sig_map_get_constructor_name() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_if_let_some_sig_sig_map_get_method_name() {

        apply_convention_fixes();
    }

    #[test]
    fn test_apply_convention_fixes_has_expected_effects() {
        // Expected effects: file_read, mutation

        let _ = apply_convention_fixes();
    }

}
